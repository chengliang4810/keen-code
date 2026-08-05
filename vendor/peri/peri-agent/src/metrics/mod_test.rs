use super::*;

#[test]
fn test_truncate_short_string_unchanged() {
    let val = serde_json::json!({"error": "short"});
    let result = truncate_json_strings(val);
    assert_eq!(result["error"], "short");
}

#[test]
fn test_truncate_long_string() {
    let long: String = "x".repeat(600);
    let val = serde_json::json!({"error": long});
    let result = truncate_json_strings(val);
    assert_eq!(result["error"].as_str().unwrap().chars().count(), 500);
}

#[test]
fn test_truncate_cjk_string() {
    let long: String = "你".repeat(600);
    let val = serde_json::json!({"error": long});
    let result = truncate_json_strings(val);
    assert_eq!(result["error"].as_str().unwrap().chars().count(), 500);
}

#[test]
fn test_truncate_nested_object() {
    let long: String = "a".repeat(600);
    let val = serde_json::json!({"data": {"nested": long, "ok": "short"}, "arr": [long]});
    let result = truncate_json_strings(val);
    assert_eq!(
        result["data"]["nested"].as_str().unwrap().chars().count(),
        500
    );
    assert_eq!(result["data"]["ok"], "short");
    assert_eq!(result["arr"][0].as_str().unwrap().chars().count(), 500);
}

#[test]
fn test_truncate_non_string_unchanged() {
    let val = serde_json::json!({"count": 42, "flag": true, "null": null});
    let result = truncate_json_strings(val);
    assert_eq!(result["count"], 42);
    assert_eq!(result["flag"], true);
    assert!(result["null"].is_null());
}

#[test]
fn test_today_format() {
    let date = today();
    assert_eq!(date.len(), 10);
    assert!(date.contains('-'));
}

#[test]
#[cfg_attr(not(unix), ignore = "RSS measurement only supported on Unix")]
fn test_current_rss_mb_returns_positive_on_unix() {
    let rss = current_rss_mb();
    assert!(rss.is_some(), "current_rss_mb() should return Some on Unix");
    assert!(rss.unwrap() > 0, "RSS should be positive");
}

#[cfg(unix)]
#[test]
fn test_current_rss_mb_is_realtime_not_monotonic_max() {
    // 验证返回的是当前 RSS（可下降），而非 ru_maxrss（单调递增）
    let baseline = current_rss_mb().expect("should get baseline RSS");
    assert!(baseline > 0);

    // 使用 mmap 分配并真实写入页面以强制 RSS 上升
    // Vec::drop → free() 在 macOS 上不归还物理页，必须用 munmap 才能验证回落
    let size = 200usize * 1024 * 1024; // 200 MB
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    assert_ne!(ptr, libc::MAP_FAILED, "mmap failed");
    // 逐页写入以触发实际物理页分配
    for i in 0..(size / 4096) {
        unsafe {
            *(ptr as *mut u8).add(i * 4096) = 1;
        }
    }
    let peak = current_rss_mb().expect("should get peak RSS after allocation");
    assert!(
        peak > baseline + 50,
        "peak RSS ({}) should be significantly > baseline ({}); got delta={}",
        peak,
        baseline,
        peak.saturating_sub(baseline)
    );

    // munmap 立即归还物理页给 OS（跨平台可靠）
    let ret = unsafe { libc::munmap(ptr, size) };
    assert_eq!(ret, 0, "munmap failed");
    let after = current_rss_mb().expect("should get RSS after free");
    // 允许 5 MB 容差（measurement overhead）
    assert!(
        after < peak.saturating_sub(50),
        "after-free RSS ({}) should be significantly < peak ({}). \
         munmap should return physical pages to the OS immediately",
        after,
        peak
    );
}
