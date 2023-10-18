#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as inner;

#[cfg(not(target_os = "linux"))]
mod non_linux;

#[cfg(not(target_os = "linux"))]
use non_linux as inner;

fn main() -> color_eyre::Result<()> {
    inner::main()
}
