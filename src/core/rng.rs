//! 随机源（API §7）。
//!
//! 阶段 1 全程显式注入随机源，禁止全局 rng（D-027 / D-050）。
//! `GameState::new` / `GameState::with_rng` 调用 `RngSource` 的方式由 D-028
//! RngSource → deck 发牌协议严格定义，testers 可基于此构造 stacked rng
//! 来产生指定牌序，无需依赖实现内部细节。

/// 显式注入的随机源。所有用到随机数的地方都必须接受 `&mut dyn RngSource`，
/// 禁止使用全局 rng。
///
/// `Send` 约束：阶段 1 多线程模拟（D-054）要求 `RngSource` 可在线程间转移；
/// 实现方必须满足 `Send`。`Sync` 不强制（每线程持有独占 rng）。
pub trait RngSource: Send {
    fn next_u64(&mut self) -> u64;
}

/// 标准实现：基于 ChaCha20，seed-determined。
///
/// `from_seed` 必须确定性：相同 seed 在所有平台上产生相同序列
/// （ChaCha20 算法保证）。禁止使用 `OsRng` / `thread_rng()` 等系统熵源
/// 进入规则引擎或评估器。
pub struct ChaCha20Rng {
    /// 内部状态由 B2 阶段填入（接 `rand_chacha::ChaCha20Rng`）。
    /// 当前为占位以保持 `pub` API 不可外部构造。
    _placeholder: (),
}

impl ChaCha20Rng {
    pub fn from_seed(seed: u64) -> Self {
        let _ = seed;
        unimplemented!()
    }
}

impl RngSource for ChaCha20Rng {
    fn next_u64(&mut self) -> u64 {
        unimplemented!()
    }
}

/// 适配器：把任意 `rand::RngCore` 包装成 `RngSource`。
///
/// 不使用 blanket impl（会与具名 [`ChaCha20Rng`] 冲突，且无法附加 `Send` 约束）。
pub struct RngCoreAdapter<R: rand::RngCore + Send>(pub R);

impl<R: rand::RngCore + Send> RngCoreAdapter<R> {
    pub fn from_rng_core(inner: R) -> Self {
        RngCoreAdapter(inner)
    }
}

impl<R: rand::RngCore + Send> RngSource for RngCoreAdapter<R> {
    fn next_u64(&mut self) -> u64 {
        unimplemented!()
    }
}
