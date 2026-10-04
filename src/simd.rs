//! Where the vector code comes from: the lossless transforms are written
//! as loops the compiler vectorises, and [`with_wide_vectors`] compiles
//! them for AVX2 when the processor has it. They are integer arithmetic,
//! so results do not depend on the processor. The `force-scalar` feature
//! turns the AVX2 variant off.

#![allow(unsafe_code)]

/// Runs `f` compiled for AVX2 where the processor has it (`f` and what it
/// inlines are vectorised 32 bytes wide), else as it is.
#[inline(always)]
pub(crate) fn with_wide_vectors<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(all(target_arch = "x86_64", not(feature = "force-scalar")))]
    if std::arch::is_x86_feature_detected!("avx2") {
        #[target_feature(enable = "avx2")]
        fn avx2<R>(f: impl FnOnce() -> R) -> R {
            f()
        }
        // SAFETY: AVX2 was detected.
        return unsafe { avx2(f) };
    }
    f()
}
