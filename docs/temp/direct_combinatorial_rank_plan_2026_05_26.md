# 直接组合数排名（Direct Combinatorial Rank）优化计划

## 目标
替换 `src/abstraction/canonical_enum.rs` 中占用 2.2GB 内存的 Lazy 构建表机制，转而使用 $O(1)$ 的直接组合数学公式计算（Direct Combinatorial Rank）算法。这将在消除极大的内存开销（River 需要 ~1.97 GB）以及启动延迟（River 建表需 ~3 分钟）的同时，保证生成的 canonical observation ID 与现有的枚举遍历算法完全一致。

## 背景与数学基础
Waugh 的遍历算法采用字典序（Lexicographical）对等价类形式进行排序：
1. **形状（Shapes）**：将 4 个花色在牌面和手牌中的分布数量作为其特征，例如 `(b_count, h_count)`。将所有花色的分布统计汇总在一起，在消除对称性后按升序排列，即构成了一个宏观“形状”。
2. **同组内的掩码（Masks within a group）**：如果多个花色具有相同的分布特征 `(b_count, h_count)`，它们便组成了一个多重集组（multiset group）。为了打破这些花色之间的对称性，组内每个花色选取的 `(b_mask, h_mask)` 组合在字典序上必须单调非降。
3. **位掩码与逆字典序（Colexicographical Order）**：现有的代码利用 Gosper's hack 枚举了所有的 `b_mask` 和 `h_mask`，其生成的掩码在数值上是严格递增的。掩码数值上的单调递增，在组合数学中精确等同于**逆字典序（Colexicographical Rank）**。

因此，任何 `(board, hole)` 在等价类签名中的精确索引，可以直接通过组合数学拆解后直接求得，无需搜索或查表：
1. `形状排名 (Shape Rank)`：在所有比当前形状（Shape）字典序更小的形状下，总共包含多少个具体的等价手牌数量。
2. `掩码排名 (Mask Rank)`：在没有交集的合法掩码组合内，`(b_mask, h_mask)` 的具体逆字典序排名。
3. `多重集排名 (Multiset Rank)`：同一个对称组内，一串有序分配结果 $R_1 \le R_2 \le \dots \le R_k$ 所处的具体字典序排名。

## 方案改动设计

### 1. 核心文件 `src/abstraction/canonical_enum.rs`

#### A. 添加组合数学底层原语
添加用于组合运算的纯函数（尽量使用 `const fn` 以便于后续如有需要在编译期生成表格）：
- `choose(n, k)`: 计算组合数 $C(n, k)$。
- `colex_rank(mask)`: 计算一个给定位掩码的逆字典序排名。
- `mask_pair_rank(b_count, h_count, b_mask, h_mask)`: 将双层掩码映射为一个密集连续的整数 $R \in [0, \binom{13}{b}\binom{13-b}{h} - 1]$。
  - `b_rank = colex_rank(b_mask)`
  - `mapped_h = map_bits(h_mask, b_mask)` (将 `h_mask` 压缩至 $13-b$ 的可用剩余比特位中)
  - `h_rank = colex_rank(mapped_h)`
  - `rank = b_rank * choose(13 - b_count, h_count) + h_rank`
- `multiset_lex_rank(ranks, N)`: 计算有序序列 $R_1 \le R_2 \le \dots \le R_k$ 在 $[0, N-1]$ 值域内的字典序排名。

#### B. 预计算极小规模的 Shape 偏移表 (Tiny Lazy Table)
我们不再需要包含 1.23 亿条目的 `Vec<u128>`，只需要一个极小的 `Vec<(Shape, u32)>` 来将每个合法的 Shape 映射至其起始偏移量。
- `Shape` 使用 `[(u8, u8); 4]` 表示。
- 全局存在的合法形状总数极其少（例如，对于 5 张公共牌的情况，通常 < 100 种）。
- 我们可以保留现有的 `enumerate_shapes` 逻辑来延迟初始化这个偏移表，但直接跳过耗时的掩码展开流程。取而代之的是，利用公式在常数时间算出形状的容量：
  - `shape_size = product( choose(N_group + k_group - 1, k_group) for each group )`
  - 这张微型懒加载表仅占用几十字节内存，构建耗时不足 1 毫秒。

#### C. 重写 `canonical_observation_id`
```rust
pub fn canonical_observation_id(street: StreetTag, board: &[Card], hole: [Card; 2]) -> u32 {
    // 1. 打包并排序得到规范形式的 Shape 和 Masks
    // 2. 从极小的微型形状偏移表 (Tiny Shape Table) 查出基准偏移量 `shape_offset`
    // 3. 针对每一个共享相同 (b_count, h_count) 的花色组：
    //    a. 算出组内每个花色的 mask_pair_rank
    //    b. 算出整个组序列的 multiset_lex_rank
    // 4. 将各组的排名与后续组的大小容量相乘，累加后即得出最终确切的 ID！
    // 5. 返回 shape_offset + sum(group_ranks)
}
```

#### D. 重写 `nth_canonical_form`
对应地，实现上述映射的逆运算（反查发牌）：
- 在极小的偏移表里二分查找定位所属的 Shape。
- 减去 `shape_offset` 获得在该 Shape 下的具体排名。
- 对每个组的多重集排名进行逆向反解算。
- 利用逆向的组合排序（Combinadic unranking）反解出 `(b_mask, h_mask)`。
- 从掩码中直接精确地重建还原手牌数据。

## 验收及验证计划
1. **字节级一致性验证 (Byte-for-Byte Equivalence)**: 运行现有的测试 `cargo test --release --test canonical_observation` 及 `cargo test --release -- abstraction::canonical_enum`。修改后基于组合算法计算的 ID **必须完全等同于**原有遍历映射产生的 $O(N)$ 索引。 
2. **性能基准验证**: 通过 `cargo bench` 确保 `canonical_observation_id` 的运算时间稳定维持在几十纳秒左右，彻底消除构建延迟，同时验证运行时常驻内存被完全释放。

## 待用户审核区
> [!IMPORTANT]
> 组合数学层面的转换极其严谨（基于 Combinadics / Colexicographical Ranking），此优化等于是用纯数学计算彻底替换了之前的暴力查表行为。考虑到其重要性，我将在这段替换过程中保留甚至添加相应的 `debug_assert!` 验证语句，将组合逻辑的内部行为与现存的枚举流程严格挂钩校验，避免替换后引发隐藏 bug。
>
> 计划已翻译完毕。一旦您确认同意，我即刻动手开始替换实现。
