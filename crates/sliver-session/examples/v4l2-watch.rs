//! Prints who holds a V4L2 camera node open (`/dev/video*`), as it changes. Read-only.
fn main() {
    sliver_session::capture::v4l2::watch(
        std::path::Path::new("/dev"),
        std::path::Path::new("/proc"),
        |apps| {
            println!("{apps:?}");
            true
        },
    );
}
