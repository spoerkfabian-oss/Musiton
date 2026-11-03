//! Verifies the allocation guard is wired up and lets non-allocating code run.

use musiton_engine::rt::{no_alloc, AllocDisabler};

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

#[test]
fn non_allocating_code_runs_under_guard() {
    let mut buf = vec![0.0_f32; 256];
    let sum = no_alloc(|| {
        for (i, s) in buf.iter_mut().enumerate() {
            *s = i as f32;
        }
        buf.iter().sum::<f32>()
    });
    assert_eq!(sum, (0..256).sum::<i32>() as f32);
}
