use anyhow::bail;

pub(crate) fn finalize_board_test_run(suite_name: &str, failed: &[String]) -> anyhow::Result<()> {
    if failed.is_empty() {
        println!("all {suite_name} board test groups passed");
        Ok(())
    } else {
        bail!(
            "{suite_name} board tests failed for {} group(s): {}",
            failed.len(),
            failed.join(", ")
        )
    }
}
