//! Windows按需读取TCP监听表，不启动lsof/netstat、扫描网页或后台探测。
use super::Server;

#[cfg(windows)]
use std::{
    collections::{BTreeMap, HashMap},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

#[cfg(windows)]
#[derive(Debug)]
struct Listener {
    pid: u32,
    host: IpAddr,
    port: u16,
    family: &'static str,
}

#[cfg(windows)]
fn listeners() -> Result<Vec<Listener>, String> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
        TCP_TABLE_OWNER_PID_LISTENER,
    };
    let mut listeners = Vec::new();
    for (family, row_size) in [
        (2u32, std::mem::size_of::<MIB_TCPROW_OWNER_PID>()),
        (23u32, std::mem::size_of::<MIB_TCP6ROW_OWNER_PID>()),
    ] {
        let mut length = 0u32;
        // 系统API的首调用查询大小；缓冲表可能在两次调用间变化，最多重试3次。
        unsafe {
            GetExtendedTcpTable(
                std::ptr::null_mut(),
                &mut length,
                0,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            );
        }
        let mut table = None;
        for _ in 0..3 {
            if !(4..=16 * 1024 * 1024).contains(&length) {
                return Err("TCP监听表大小无效".into());
            }
            let mut bytes = vec![0u8; length as usize];
            let code = unsafe {
                GetExtendedTcpTable(
                    bytes.as_mut_ptr().cast(),
                    &mut length,
                    0,
                    family,
                    TCP_TABLE_OWNER_PID_LISTENER,
                    0,
                )
            };
            if code == 122 {
                continue;
            }
            if code != 0 {
                return Err(format!("TCP监听查询失败，系统错误{code}"));
            }
            table = Some(bytes);
            break;
        }
        let bytes = table.ok_or("TCP监听表持续变化，请重试")?;
        let count = u32::from_ne_bytes(bytes[..4].try_into().unwrap()) as usize;
        if count > (bytes.len() - 4) / row_size {
            return Err("TCP监听表长度不匹配".into());
        }
        for index in 0..count {
            let offset = 4 + index * row_size;
            let (pid, host, port) = if family == 2 {
                // Vec<u8>没有C结构对齐保证，使用read_unaligned，且读取前已校验完整边界。
                let row = unsafe {
                    bytes
                        .as_ptr()
                        .add(offset)
                        .cast::<MIB_TCPROW_OWNER_PID>()
                        .read_unaligned()
                };
                (
                    row.dwOwningPid,
                    IpAddr::V4(Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes())),
                    u16::from_be(row.dwLocalPort as u16),
                )
            } else {
                let row = unsafe {
                    bytes
                        .as_ptr()
                        .add(offset)
                        .cast::<MIB_TCP6ROW_OWNER_PID>()
                        .read_unaligned()
                };
                (
                    row.dwOwningPid,
                    IpAddr::V6(Ipv6Addr::from(row.ucLocalAddr)),
                    u16::from_be(row.dwLocalPort as u16),
                )
            };
            if pid > 0 && port > 0 && (host.is_loopback() || host.is_unspecified()) {
                listeners.push(Listener {
                    pid,
                    host,
                    port,
                    family: if family == 2 { "tcp4" } else { "tcp6" },
                });
            }
        }
    }
    Ok(listeners)
}

#[cfg(windows)]
fn processes() -> sysinfo::System {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};
    System::new_with_specifics(
        RefreshKind::nothing()
            .with_processes(ProcessRefreshKind::nothing().with_cwd(UpdateKind::Always)),
    )
}

/// 父链只在本次进程快照内判断；限制长度及循环，不扩大为停止外部程序的权限。
#[cfg(any(windows, test))]
pub(super) fn owned_by(pid: u32, root: u32, parents: &std::collections::HashMap<u32, u32>) -> bool {
    let mut current = pid;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..64 {
        if current == root {
            return true;
        }
        if !seen.insert(current) {
            return false;
        }
        let Some(parent) = parents.get(&current) else {
            return false;
        };
        current = *parent;
    }
    false
}

#[cfg(windows)]
pub(super) fn list(servers: &[Server]) -> Result<serde_json::Value, String> {
    let system = processes();
    let parents: HashMap<_, _> = system
        .processes()
        .iter()
        .filter_map(|(id, p)| p.parent().map(|parent| (id.as_u32(), parent.as_u32())))
        .collect();
    let mut rows: BTreeMap<u32, Vec<Listener>> = BTreeMap::new();
    for item in listeners()? {
        rows.entry(item.pid).or_default().push(item);
    }
    if rows.len() > 512 {
        return Err("本机服务数量超过显示上限".into());
    }
    let values: Vec<_> = rows.into_iter().map(|(pid, rows)| {
        let process = system.process(sysinfo::Pid::from_u32(pid));
        let run = servers.iter().find(|run| owned_by(pid, run.pid, &parents));
        let name = process.map(|p| p.name().to_string_lossy().into_owned()).unwrap_or_else(|| format!("PID {pid}"));
        let command = run.map(|s| s.command.clone()).unwrap_or_else(|| name.clone());
        // 不读取外部进程的环境/完整参数，参数可能含凭据；原显示契约允许空args。
        let addresses: Vec<_> = rows.iter().map(|row| serde_json::json!({"host":row.host.to_string(),"port":row.port,"family":row.family,
            // TCP监听不能证明HTTP协议；只有受管理脚本明确声明的origin才可打开。
            "url":run.and_then(|s| s.announced.lock().get(&row.port).cloned())})).collect();
        let mut ports: Vec<_> = rows.iter().map(|r| r.port).collect(); ports.sort_unstable(); ports.dedup();
        let mut value = serde_json::json!({"id":format!("local:{pid}"),"pid":pid,"command":command,"displayName":name,"args":"", "ports":ports,"addresses":addresses,"isStoppable":run.is_some()});
        if let Some(ppid) = parents.get(&pid).filter(|p| **p > 0) { value["ppid"] = (*ppid).into(); }
        let cwd = run.map(|s| s.cwd.clone()).or_else(|| process.and_then(|p| p.cwd()).map(crate::path_utils::path_to_frontend));
        if let Some(cwd) = cwd { value["cwd"] = cwd.into(); }
        if run.is_none() { value["stopDisabledReason"] = "此进程由外部程序启动，KeenCode不持有其生命周期".into(); }
        value
    }).collect();
    Ok(serde_json::json!({"generatedAt":chrono::Utc::now().to_rfc3339(),"servers":values}))
}

#[cfg(windows)]
pub(super) fn owner(servers: &[Server], pid: u32, port: u16) -> Result<String, String> {
    if !listeners()?
        .iter()
        .any(|row| row.pid == pid && row.port == port)
    {
        return Err("此服务端口已退出或归属改变".into());
    }
    let system = processes();
    let parents = system
        .processes()
        .iter()
        .filter_map(|(id, p)| p.parent().map(|parent| (id.as_u32(), parent.as_u32())))
        .collect();
    servers
        .iter()
        .find(|s| owned_by(pid, s.pid, &parents))
        .map(|s| s.project_id.clone())
        .ok_or("此服务不由KeenCode管理，不能停止外部进程".into())
}

#[cfg(not(windows))]
pub(super) fn list(_: &[Server]) -> Result<serde_json::Value, String> {
    Err("此平台尚未对接本机TCP服务查询".into())
}
#[cfg(not(windows))]
pub(super) fn owner(_: &[Server], _: u32, _: u16) -> Result<String, String> {
    Err("此平台尚未对接本机TCP服务归属查询".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parent_chain_rejects_unrelated_cycles_and_missing_parent() {
        let parents = [(30, 20), (20, 10), (40, 41), (41, 40)].into();
        assert!(owned_by(30, 10, &parents));
        assert!(owned_by(10, 10, &parents));
        assert!(!owned_by(40, 10, &parents));
        assert!(!owned_by(99, 10, &parents));
    }
}
