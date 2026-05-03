use std::path::Path;

pub(crate) fn skip_test(path: &Path) -> bool {
    let path_str = path.to_str().unwrap_or_default();
    if path_str.contains("paris/eip7610_create_collision") {
        return true;
    }

    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    matches!(
        name,
        "RevertInCreateInInit_Paris.json"
            | "RevertInCreateInInit.json"
            | "dynamicAccountOverwriteEmpty.json"
            | "dynamicAccountOverwriteEmpty_Paris.json"
            | "RevertInCreateInInitCreate2Paris.json"
            | "create2collisionStorage.json"
            | "RevertInCreateInInitCreate2.json"
            | "create2collisionStorageParis.json"
            | "InitCollision.json"
            | "InitCollisionParis.json"
            | "test_init_collision_create_opcode.json"
            | "ValueOverflow.json"
            | "ValueOverflowParis.json"
            | "Call50000_sha256.json"
            | "static_Call50000_sha256.json"
            | "loopMul.json"
            | "CALLBlake2f_MaxRounds.json"
    )
}
