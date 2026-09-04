//! 稠密位图（`Vec<u64>` 实现）。
//!
//! # 为什么自研而不引 `roaring`
//!
//! `ChunkId` / `DocId` 都是 `u32` **连续自增**（见 `types.rs`），ID 空间稠密：
//!
//! - 内存：10 万 ID → 12.5 KB（`Vec<u64>`），远小于任何压缩位图的常量开销；
//! - 速度：位运算是 O(1) 直线寻址，没有 Roaring 的 container 跳转与分派；
//! - `count_ones()` O(1)（缓存 `count`），供向量路 `ef` 策略与空集短路决策使用；
//! - 集合求值与迭代序无关，天然满足 NFR-06 确定性。
//!
//! 代价：`clear` 不清零内存，位图长度只增不减。ID 空间**稀疏**时本结构不适用。
//!
//! # `count` 的维护契约
//!
//! `count` 是 `words` 中置位数的**缓存**，必须与之一致。漂移会让 `is_empty()`
//! 与 `count_ones()` 静默返回错误结果，因此：
//!
//! - `set` / `clear` 先判断原位再决定是否 ±1（不可无条件自增/自减）；
//! - `union_with` / `intersect_with` 按**逐字增量**维护（不是 `a + b` 这种近似）；
//! - 两者末尾做 `assert_eq!`（仅 debug 构建）防漂移。
//!
//! `set` / `clear` 是单比特操作，正确性由 `tests` 中的 T4 直接覆盖，
//! **不在热路径上做全量重算**（大批量构建时会退化成 O(N²)）。

/// 稠密位图（ID 为 `u32` 连续分配场景）。
///
/// 语义上是一个 `HashSet<u32>`，但用位存储。两个位图**表示的集合相同即相等**，
/// 与内部 `words` 的长度无关（见 [`PartialEq`] 实现）——否则
/// 「先 set(0)」与「先 set(127) 再 clear(127)」会不相等。
#[derive(Debug, Clone, Default)]
pub struct Bitmap {
    /// 位存储，低位字在前（第 `i` 位 = 第 `i/64` 字的第 `i%64` 位）
    words: Vec<u64>,
    /// 置位计数缓存，恒等于 `words` 中的置位总数
    count: usize,
}

/// chunk 级位图（语义别名，编译期同类型）
pub type ChunkBits = Bitmap;

/// doc 级位图（语义别名，编译期同类型）
pub type DocBits = Bitmap;

impl Bitmap {
    /// 空位图（不分配内存）
    pub fn new() -> Self {
        Self::default()
    }

    /// 置位 `id`（自动扩容；已在位则 no-op）。
    pub fn set(&mut self, id: u32) {
        let (wi, bit) = (id / 64, id % 64);
        let need = wi as usize + 1;
        if need > self.words.len() {
            self.words.resize(need, 0);
        }
        let mask = 1u64 << bit;
        if self.words[wi as usize] & mask == 0 {
            self.words[wi as usize] |= mask;
            self.count += 1;
        }
    }

    /// 清位 `id`（越界或未在位则 no-op）。
    pub fn clear(&mut self, id: u32) {
        let (wi, bit) = (id / 64, id % 64);
        if let Some(word) = self.words.get_mut(wi as usize) {
            let mask = 1u64 << bit;
            if *word & mask != 0 {
                *word &= !mask;
                self.count -= 1;
            }
        }
    }

    /// `id` 是否在位（越界返回 `false`）。
    pub fn contains(&self, id: u32) -> bool {
        let (wi, bit) = (id / 64, id % 64);
        match self.words.get(wi as usize) {
            Some(w) => w & (1u64 << bit) != 0,
            None => false,
        }
    }

    /// 并集（`self |= other`）。
    pub fn union_with(&mut self, other: &Self) {
        if other.words.len() > self.words.len() {
            self.words.resize(other.words.len(), 0);
        }
        for (i, word) in self.words.iter_mut().enumerate() {
            let before = *word;
            *word |= other.words.get(i).copied().unwrap_or(0);
            // 只累加本次新置起的位，不能写成 a + b
            self.count += (*word & !before).count_ones() as usize;
        }
        self.debug_check();
    }

    /// 交集（`self &= other`）。
    pub fn intersect_with(&mut self, other: &Self) {
        for (i, word) in self.words.iter_mut().enumerate() {
            let before = *word;
            *word &= other.words.get(i).copied().unwrap_or(0);
            // 只扣减本次被清掉的位
            self.count -= (before & !*word).count_ones() as usize;
        }
        self.debug_check();
    }

    /// 置位总数（O(1)，返回缓存值）。
    pub fn count_ones(&self) -> usize {
        self.count
    }

    /// 是否为空集（O(1)）。
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// 按升序迭代所有在位的 ID。
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.words.iter().enumerate().flat_map(|(wi, &word)| {
            let mut word = word;
            let base = wi as u32 * 64;
            std::iter::from_fn(move || {
                if word == 0 {
                    None
                } else {
                    let bit = word.trailing_zeros();
                    word &= word - 1;
                    Some(base + bit)
                }
            })
        })
    }

    /// 该位图覆盖的 ID 上界（不含），即 `words.len() * 64`。
    ///
    /// 仅供测试与诊断使用；不代表最高置位 ID。
    pub fn capacity_bits(&self) -> usize {
        self.words.len() * 64
    }

    /// 逐字重算置位数（**仅 debug 构建存在**；O(words)）。
    #[cfg(debug_assertions)]
    fn count_ones_slow(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// 校验 `count` 与位内容一致（仅 debug 构建生效，release 是空函数）。
    ///
    /// 只在 `union_with` / `intersect_with` 末尾调用——这两处的 `count` 增量
    /// 公式是真正的漂移风险点；`set` / `clear` 为单比特操作，由单测直接覆盖，
    /// 若在批量构建路径上逐次重算会把 O(N) 变成 O(N²)。
    #[inline]
    fn debug_check(&self) {
        #[cfg(debug_assertions)]
        assert_eq!(
            self.count,
            self.count_ones_slow(),
            "Bitmap::count 与位内容漂移"
        );
    }
}

impl PartialEq for Bitmap {
    /// 按**集合语义**比较：公有前缀逐字相等，且各自的多余高位全为 0。
    ///
    /// 不能用 `derive(PartialEq)`——那会把 `words` 的长度差异也算作不相等，
    /// 而长度取决于「历史上设置过的最大 ID」，与集合内容无关。
    fn eq(&self, other: &Self) -> bool {
        let n = self.words.len().min(other.words.len());
        if self.words[..n] != other.words[..n] {
            return false;
        }
        self.words[n..].iter().all(|w| *w == 0) && other.words[n..].iter().all(|w| *w == 0)
    }
}

impl Eq for Bitmap {}

#[cfg(test)]
mod tests {
    #![allow(non_snake_case)]
    use super::*;

    #[test]
    fn 空位图不含任何位() {
        let b = Bitmap::new();
        assert!(b.is_empty());
        assert_eq!(b.count_ones(), 0);
        assert!(!b.contains(0));
        assert!(!b.contains(1000));
        assert_eq!(b.iter().count(), 0);
    }

    #[test]
    fn set与contains跨字边界() {
        let mut b = Bitmap::new();
        for id in [0u32, 1, 63, 64, 65, 127, 128, 1000] {
            b.set(id);
        }
        for id in [0u32, 1, 63, 64, 65, 127, 128, 1000] {
            assert!(b.contains(id), "id={id} 应在位");
        }
        for id in [2u32, 62, 66, 126, 129, 999] {
            assert!(!b.contains(id), "id={id} 不应在位");
        }
        assert_eq!(b.count_ones(), 8);
        assert!(b.capacity_bits() >= 1001);
    }

    #[test]
    fn set幂等不重复计数() {
        let mut b = Bitmap::new();
        b.set(7);
        b.set(7);
        b.set(7);
        assert_eq!(b.count_ones(), 1, "重复 set 不应重复计数");
    }

    #[test]
    fn clear幂等且不panic于越界() {
        let mut b = Bitmap::new();
        b.set(3);
        b.clear(3);
        b.clear(3);
        b.clear(9999); // 越界：no-op，不 panic
        assert_eq!(b.count_ones(), 0);
        assert!(b.is_empty());
    }

    #[test]
    fn set后clear回到空集() {
        let mut b = Bitmap::new();
        for id in 0..300u32 {
            b.set(id);
        }
        assert_eq!(b.count_ones(), 300);
        for id in 0..300u32 {
            b.clear(id);
        }
        assert_eq!(b.count_ones(), 0);
        assert!(b.is_empty());
    }

    #[test]
    fn 迭代按升序且无重复() {
        let mut b = Bitmap::new();
        for id in [5u32, 0, 200, 63, 64, 1] {
            b.set(id);
        }
        let got: Vec<u32> = b.iter().collect();
        assert_eq!(got, vec![0, 1, 5, 63, 64, 200]);
    }

    #[test]
    fn 并集去重计数正确() {
        let mut a = Bitmap::new();
        a.set(1);
        a.set(2);
        a.set(100);
        let mut b = Bitmap::new();
        b.set(2);
        b.set(3);
        b.set(200);
        a.union_with(&b);
        assert_eq!(a.count_ones(), 5, "两集合去重后应为 5 个");
        for id in [1u32, 2, 3, 100, 200] {
            assert!(a.contains(id));
        }
    }

    #[test]
    fn 交集计数正确() {
        let mut a = Bitmap::new();
        for id in [1u32, 2, 3, 100, 200] {
            a.set(id);
        }
        let mut b = Bitmap::new();
        for id in [2u32, 3, 200, 300] {
            b.set(id);
        }
        a.intersect_with(&b);
        assert_eq!(a.count_ones(), 3);
        for id in [2u32, 3, 200] {
            assert!(a.contains(id));
        }
        assert!(!a.contains(1));
        assert!(!a.contains(100));
    }

    #[test]
    fn 交集为空时is_empty为真() {
        let mut a = Bitmap::new();
        a.set(1);
        let mut b = Bitmap::new();
        b.set(2);
        a.intersect_with(&b);
        assert!(a.is_empty());
        assert_eq!(a.count_ones(), 0);
    }

    #[test]
    fn 与空集运算不改变结果() {
        let mut a = Bitmap::new();
        a.set(10);
        let empty = Bitmap::new();
        a.union_with(&empty);
        assert_eq!(a.count_ones(), 1);
        a.intersect_with(&Bitmap::new());
        assert!(a.is_empty());
    }

    #[test]
    fn 相等性按集合语义与words长度无关() {
        let mut short = Bitmap::new();
        short.set(0);
        let mut long = Bitmap::new();
        long.set(200); // 撑到 4 个字
        long.clear(200);
        long.set(0);
        assert!(
            long.capacity_bits() > short.capacity_bits(),
            "构造前提：两者 words 长度应不同"
        );
        assert_eq!(short, long, "集合相同即相等，与 words 长度无关");
    }

    #[test]
    fn 跨字边界的并集与交集() {
        let mut a = Bitmap::new();
        for id in 0..130u32 {
            a.set(id);
        }
        let mut b = Bitmap::new();
        for id in 60..200u32 {
            b.set(id);
        }
        a.union_with(&b);
        assert_eq!(a.count_ones(), 200);
        a.intersect_with(&b);
        assert_eq!(a.count_ones(), 140, "60..200 共 140 个");
    }

    #[test]
    fn 随机增删后count与位内容一致() {
        // 确定性伪随机（xorshift），避免引入 rand 依赖
        let mut state = 0x2545F4914F6CDD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut b = Bitmap::new();
        let mut model = std::collections::HashSet::new();
        for _ in 0..5000 {
            let id = (next() % 500) as u32;
            if next() % 2 == 0 {
                b.set(id);
                model.insert(id);
            } else {
                b.clear(id);
                model.remove(&id);
            }
        }
        assert_eq!(b.count_ones(), model.len(), "count 与模型集合大小一致");
        for id in 0..500u32 {
            assert_eq!(b.contains(id), model.contains(&id), "id={id} 不一致");
        }
        let got: Vec<u32> = b.iter().collect();
        let mut want: Vec<u32> = model.into_iter().collect();
        want.sort_unstable();
        assert_eq!(got, want, "迭代结果与模型一致且升序");
    }
}
