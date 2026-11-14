//! R1.9 黄金 fixtures 路径约定:wr-server/tests 里的 parity/e2e 必须
//! 能按相对路径触达仓库根的 `testdata/golden/`(R0 固化的黄金样本)。
//! 本测试钉住目录结构 —— 布局漂移(移动 backend/ 或 testdata/)
//! 会在 CI 立刻失败,而不是等到 R2 parity 才暴露。

use std::path::PathBuf;

#[test]
fn golden_fixture_dir_reachable_from_crate() {
    let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../testdata/golden");
    let manifest = golden.join("manifest.json");
    assert!(
        manifest.is_file(),
        "黄金样本清单不可达: {}(backend/ 与 testdata/ 的相对布局被改动?)",
        manifest.display()
    );
    let cases = golden.join("cases");
    assert!(cases.is_dir(), "黄金样本目录缺失: {}", cases.display());
}

#[test]
fn sqlite_snapshot_fixture_reachable() {
    // C5 兼容性测试夹具(R0.4),R4 直读现有 db 的基准。
    let snapshot =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../testdata/golden/sqlite-snapshot");
    assert!(
        snapshot.join("schema.sql").is_file(),
        "sqlite schema 快照缺失: {}",
        snapshot.display()
    );
    assert!(
        snapshot.join("rows.json").is_file(),
        "sqlite rows 快照缺失: {}",
        snapshot.display()
    );
}
