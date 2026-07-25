#[cfg(windows)]
fn main() {
    let icon = "../../assets/screen-mirror.ico";
    if std::path::Path::new(icon).exists() {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon(icon);
        resource
            .compile()
            .expect("failed to embed Windows resources");
    }
}

#[cfg(not(windows))]
fn main() {}
