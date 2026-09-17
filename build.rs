fn main() {
    #[cfg(target_os = "windows")]
    {
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .compile()
            .expect("failed to embed icon resource");
        // Для инструмента, которому нужны повышенные права, добавьте
        // assets/app.manifest (см. agent-control-rs) и замените вызов на:
        // winresource::WindowsResource::new()
        //     .set_icon("assets/icon.ico")
        //     .set_manifest_file("assets/app.manifest")
        //     .compile()
        //     .expect("failed to embed icon/manifest resources");
    }
}
