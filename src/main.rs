use std::process::ExitCode;

// musl's allocator takes one global lock: with it the container served about 12 000 object reads
// per second whether 8 or 128 connections asked, against 66 000 to 189 000 with this one. Other
// targets keep their C library's allocator, which already scales.
#[cfg(target_env = "musl")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// With huge pages set to `always`, as in Docker Desktop's VM, one write into the allocator's
/// regions makes 2 MiB resident: the process held 15 MiB idle and 78 MiB after a load, against
/// 8 MiB and 32 MiB without them, at the same speed. Failing only costs that memory, so the result
/// is not checked.
#[cfg(target_env = "musl")]
#[expect(
    unsafe_code,
    reason = "std has no wrapper for prctl; the call passes only integers"
)]
fn disable_transparent_huge_pages() {
    const DISABLE: libc::c_ulong = 1;
    const UNUSED: libc::c_ulong = 0;
    // SAFETY: this option reads no memory and changes only this process's paging policy. The
    // kernel takes the variadic arguments as unsigned longs and rejects unused ones that are not
    // zero, so they are passed at exactly that width.
    let _ = unsafe { libc::prctl(libc::PR_SET_THP_DISABLE, DISABLE, UNUSED, UNUSED, UNUSED) };
}

fn main() -> ExitCode {
    #[cfg(target_env = "musl")]
    disable_transparent_huge_pages();
    gcs_mimic::app::main()
}
