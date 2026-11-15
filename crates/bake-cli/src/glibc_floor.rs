//! Keeps the Linux executable within its glibc floor, GLIBC_2.34.
//!
//! glibc 2.35 gave `hypotf` a new symbol version, and glibc 2.43 gave
//! `acosf` and `atan2f` theirs. A build linked on a newer host binds those
//! versions, and the loader then refuses to start `bake` on an older glibc.
//! The SVG renderer reaches them through `f32::acos`, `f32::atan2`, and
//! `f32::hypot`. The executable defines the three functions itself from
//! `libm`, the Rust port of musl's, so the linker resolves them here and
//! needs no glibc version for them. `bake-release glibc-floor` checks every
//! release build.

#[unsafe(no_mangle)]
extern "C" fn acosf(x: f32) -> f32 {
    libm::acosf(x)
}

#[unsafe(no_mangle)]
extern "C" fn atan2f(y: f32, x: f32) -> f32 {
    libm::atan2f(y, x)
}

#[unsafe(no_mangle)]
extern "C" fn hypotf(x: f32, y: f32) -> f32 {
    libm::hypotf(x, y)
}

#[cfg(test)]
mod tests {
    use std::hint::black_box;

    // The standard library's methods reach these definitions in a test
    // build as in the executable.
    #[test]
    fn the_defined_functions_answer_as_the_standard_library_s_methods() {
        let close = |a: f32, b: f32| (a - b).abs() <= 1e-6 * b.abs().max(1.0);
        assert!(close(black_box(0.5f32).acos(), std::f32::consts::FRAC_PI_3));
        assert!(close(black_box(-1.0f32).acos(), std::f32::consts::PI));
        assert!(close(
            black_box(1.0f32).atan2(-1.0),
            3.0 * std::f32::consts::FRAC_PI_4
        ));
        assert!(close(black_box(0.0f32).atan2(-1.0), std::f32::consts::PI));
        assert!(close(black_box(3.0f32).hypot(4.0), 5.0));
        assert!(black_box(2.0f32).acos().is_nan());
        assert_eq!(black_box(f32::INFINITY).hypot(f32::NAN), f32::INFINITY);
    }
}
