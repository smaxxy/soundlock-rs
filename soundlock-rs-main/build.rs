fn main() {
    embed_resource::compile("app.rc", embed_resource::NONE)
        .manifest_required()
        .expect("failed to compile Sound Lock icon and visual-style manifest");
}
