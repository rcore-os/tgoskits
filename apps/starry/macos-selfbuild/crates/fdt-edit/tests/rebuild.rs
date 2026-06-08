//! FDT Rebuild 测试
//!
//! 使用 diff 命令验证 parse 和 encode 的正确性：
//! 1. 从 DTB 解析为 Fdt 对象
//! 2. 从 Fdt 对象编码回 DTB
//! 3. 使用 dtc 将两个 DTB 都反编译为 DTS
//! 4. 使用 diff 对比两个 DTS，确保语义一致

#[cfg(target_os = "linux")]
use dtb_file::*;
#[cfg(target_os = "linux")]
use fdt_edit::*;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::process::Command;

/// 测试用例
struct DtbTestCase {
    name: &'static str,
    loader: fn() -> Align4Vec,
}

/// 所有测试用例
const TEST_CASES: &[DtbTestCase] = &[
    DtbTestCase {
        name: "qemu",
        loader: || fdt_qemu(),
    },
    DtbTestCase {
        name: "pi_4b",
        loader: || fdt_rpi_4b(),
    },
    DtbTestCase {
        name: "phytium",
        loader: || fdt_phytium(),
    },
    DtbTestCase {
        name: "rk3568",
        loader: || fdt_3568(),
    },
    DtbTestCase {
        name: "reserve",
        loader: || fdt_reserve(),
    },
];

/// 主测试函数：遍历所有测试用例
#[test]
fn test_rebuild_all() {
    for case in TEST_CASES {
        test_rebuild_single(case);
    }
}

/// 运行单个 rebuild 测试
fn test_rebuild_single(case: &DtbTestCase) {
    println!("Testing rebuild: {}", case.name);

    // 1. 获取原始 DTB 数据
    let raw_data = (case.loader)();
    let original =
        Fdt::from_bytes(&raw_data).unwrap_or_else(|_| panic!("Failed to parse {}", case.name));

    // 2. 编码
    let encoded = original.encode();

    // 3. 保存到 /tmp
    let tmp_dir = "/tmp/fdt_rebuild_test";
    fs::create_dir_all(tmp_dir).unwrap_or_else(|_| panic!("Failed to create tmp dir"));

    let orig_dtb_path = format!("{}/{}.orig.dtb", tmp_dir, case.name);
    let enc_dtb_path = format!("{}/{}.enc.dtb", tmp_dir, case.name);
    let orig_dts_path = format!("{}/{}.orig.dts", tmp_dir, case.name);
    let enc_dts_path = format!("{}/{}.enc.dts", tmp_dir, case.name);

    fs::write(&orig_dtb_path, &raw_data[..])
        .unwrap_or_else(|_| panic!("Failed to write {}", orig_dtb_path));
    fs::write(&enc_dtb_path, &encoded[..])
        .unwrap_or_else(|_| panic!("Failed to write {}", enc_dtb_path));

    // 4. 使用 dtc 反编译为 DTS
    let dtc_status_orig = Command::new("dtc")
        .arg("-I")
        .arg("dtb")
        .arg("-O")
        .arg("dts")
        .arg("-o")
        .arg(&orig_dts_path)
        .arg(&orig_dtb_path)
        .status()
        .unwrap_or_else(|_| panic!("Failed to run dtc on original DTB"));

    assert!(dtc_status_orig.success(), "dtc failed on original DTB");

    let dtc_status_enc = Command::new("dtc")
        .arg("-I")
        .arg("dtb")
        .arg("-O")
        .arg("dts")
        .arg("-o")
        .arg(&enc_dts_path)
        .arg(&enc_dtb_path)
        .status()
        .unwrap_or_else(|_| panic!("Failed to run dtc on encoded DTB"));

    assert!(dtc_status_enc.success(), "dtc failed on encoded DTB");

    // 5. 使用 diff 对比两个 DTS
    let diff_status = Command::new("diff")
        .arg("-u")
        .arg(&orig_dts_path)
        .arg(&enc_dts_path)
        .status()
        .unwrap_or_else(|_| panic!("Failed to run diff"));

    // 6. 验证：两个 DTS 应该语义一致
    assert!(
        diff_status.success(),
        "DTS files differ for {}: run 'diff {} {}' to see details",
        case.name,
        orig_dts_path,
        enc_dts_path
    );

    println!("Rebuild test PASSED: {}", case.name);
}
