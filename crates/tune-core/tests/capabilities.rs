use serde_json::json;
use tune_core::{capabilities, nodes::Node};
fn node(os: &str, local: bool) -> Node {
    Node::from_value(&json!({"id":"sample","alias":"sample","os":os,"local_hostname":if local {"local"} else {"remote"}}), "local")
}
#[test]
fn selection_fails_closed_before_transport() {
    let n = node("windows", true);
    assert_eq!(capabilities::select(std::slice::from_ref(&n), &[]).unwrap().len(), 1);
    assert!(capabilities::select(std::slice::from_ref(&n), &["unknown".into()]).is_err());
    for patch in [json!({"capabilities":false}), json!({"status":"retired"}), json!({"os":"linux"})] {
        let mut raw = n.raw.clone();
        for (k, v) in patch.as_object().unwrap() {
            raw[k] = v.clone();
        }
        assert!(capabilities::select(&[Node::from_value(&raw, "local")], &[]).is_err());
    }
    assert!(capabilities::select(&[node("macos", false)], &[]).is_err());
}
#[test]
fn sanitize_checks_provenance_states_and_strips_extra_fields() {
    let n = node("windows", true);
    let mut v = json!({"schema":"katala_capabilities.v1","platform":"win32","inventory_status":"ok","manufacturer":"Example","model":"Model","baseboard_manufacturer":"Example","baseboard_product":"Board","bios_version":"1.0","bios_date":"2026-01-01","secure_boot":{"state":"permission_denied"},"serial":"PRIVATE","hostname":"PRIVATE"});
    let clean = capabilities::sanitize(&v, &n).unwrap();
    assert!(!clean.to_string().contains("PRIVATE"));
    for state in ["enabled", "disabled", "permission_denied", "unsupported", "error"] {
        v["secure_boot"]["state"] = json!(state);
        assert!(capabilities::sanitize(&v, &n).is_some());
    }
    v["secure_boot"]["state"] = json!("false");
    assert!(capabilities::sanitize(&v, &n).is_none());
    v["platform"] = json!("darwin");
    assert!(capabilities::sanitize(&v, &n).is_none());
    assert!(capabilities::sanitize(&json!({}), &n).is_none());
}
#[test]
fn plan_is_never_executable_even_with_successful_evidence() {
    let p = capabilities::maintenance_plan(
        &json!({"schema":"katala_capabilities_fleet.v1","nodes":[{"node_id":"sample","ok":true,"at":123,"data":{"model":"Model","bios_version":"1.0"}}]}),
    );
    for a in p["actions"].as_array().unwrap() {
        assert_eq!(a["executable"], false);
        assert!(!a["missing_requirements"].as_array().unwrap().is_empty());
    }
    assert_eq!(p["read_only_refresh_available"], true);
}
#[test]
fn transport_uses_stdin_strict_hosts_and_no_helper() {
    for os in ["macos", "windows"] {
        for local in [true, false] {
            let (cmd, args, input) = capabilities::transport(&node(os, local));
            assert!(!input.is_empty());
            assert!(args.iter().map(String::len).sum::<usize>() < 1200);
            if !local {
                assert_eq!(cmd, "ssh");
                assert!(args.contains(&"StrictHostKeyChecking=yes".into()));
            }
            assert!(!args.iter().any(|a| a.contains("helper") || a.contains("scp")));
            if os == "windows" {
                assert!(input.is_ascii());
                assert_eq!(input.iter().filter(|b| **b == b'\n').count(), 1);
            }
        }
    }
}
#[test]
fn windows_script_is_bom_ascii_without_single_quotes_or_writes() {
    let p = include_bytes!("../../../probes/win_capabilities.ps1");
    assert!(p.starts_with(&[239, 187, 191]));
    assert!(p[3..].is_ascii());
    assert!(!p.contains(&b'\''));
    let s = String::from_utf8_lossy(p);
    assert!(s.contains("-Property Manufacturer,Model"));
    for forbidden in ["SerialNumber", "UUID", "Set-", "Restart-", "Out-File"] {
        assert!(!s.contains(forbidden));
    }
}

#[test]
fn summary_does_not_infer_write_support_from_observed_inventory() {
    let n = node("windows", true);
    let v = json!({"schema":"katala_capabilities.v1","platform":"win32","inventory_status":"error","inventory_reason":"permission_denied","secure_boot":{"state":"permission_denied"}});
    let clean = capabilities::sanitize(&v, &n).unwrap();
    assert_eq!(clean["inventory_reason"], "permission_denied");
    assert_eq!(clean["capabilities"]["firmware_read"]["support"], "unavailable");
    assert_eq!(clean["capabilities"]["firmware_write"]["support"], "unknown");
    assert_eq!(clean["capabilities"]["firmware_write"]["implementation"], "unavailable");
    assert_eq!(clean["capabilities"]["secure_boot_read"]["state"], "permission_denied");
}

#[test]
fn ssh_alias_cannot_inject_options() {
    for alias in ["", "-oProxyCommand=touch", "two names", "bad\nname"] {
        let n = Node::from_value(&json!({"id":"sample","alias":alias,"os":"windows"}), "local");
        assert!(capabilities::select(&[n], &["sample".into()]).is_err());
    }
    let (_, args, _) = capabilities::transport(&node("windows", false));
    let end = args.iter().position(|a| a == "--").unwrap();
    assert_eq!(args[end + 1], "sample");
}

#[test]
fn mac_python_is_isolated_and_resolved_from_path() {
    let (cmd, args, _) = capabilities::transport(&node("macos", true));
    assert_eq!(cmd, "/usr/bin/env");
    assert_eq!(args, ["python3", "-I", "-"]);
    let (_, args, _) = capabilities::transport(&node("macos", false));
    assert!(args.last().unwrap().contains("python3 -I -"));
}
#[test]
fn action_matrix_has_specific_evidence_and_recovery() {
    let p = capabilities::maintenance_plan(
        &json!({"nodes":[{"node_id":"sample","at":123,"ok":true,"data":{"platform":"darwin","source":"system_profiler","model":"Model","bios_version":"1"}}]}),
    );
    let actions = p["actions"].as_array().unwrap();
    for action in actions {
        assert_eq!(action["executable"], false);
        assert_eq!(action["access"], "write");
        assert!(action["recovery_conditions"].as_array().unwrap().len() >= 2);
        assert_eq!(action["evidence_refs"][0]["node_id"], "sample");
        assert_eq!(action["evidence_refs"][0]["at"], 123);
        assert_eq!(action["nodes"][0]["node_id"], "sample");
        assert_eq!(action["evidence_refs"][0]["bios_version"], "1");
    }
    let bitlocker = actions.iter().find(|a| a["action"] == "bitlocker").unwrap();
    assert_eq!(bitlocker["nodes"][0]["support"], "unsupported");
    assert_eq!(bitlocker["nodes"][0]["reason"], "platform_not_applicable");
    assert_eq!(actions.iter().find(|a| a["action"] == "reboot").unwrap()["reboot"], "yes");
    assert_ne!(actions[0]["recovery_conditions"], actions[1]["recovery_conditions"]);
}

#[cfg(unix)]
#[test]
fn isolated_python_ignores_cwd_and_pythonpath_traps() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let root = std::env::temp_dir().join(format!("kt-cap-isolated-{}-{}", std::process::id(), tune_core::db::now_ms()));
    std::fs::create_dir_all(&root).unwrap();
    let marker = root.join("loaded");
    let trap = format!("open({:?}, 'w').write('loaded')\nraise RuntimeError('module trap')\n", marker.to_str().unwrap());
    for name in ["json.py", "subprocess.py", "sitecustomize.py"] {
        std::fs::write(root.join(name), &trap).unwrap();
    }
    let (cmd, args, _) = capabilities::transport(&node("macos", true));
    let mut child = Command::new(cmd)
        .args(args)
        .current_dir(&root)
        .env("PYTHONPATH", &root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"import json,subprocess\nprint(json.dumps({'isolated':True}))\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(!marker.exists(), "cwd/PYTHONPATH trap ran before isolation");
    assert!(out.status.success());
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["isolated"], true);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn ssh_effective_options_disable_implicit_mutations_and_forwardings() {
    use std::process::Command;
    let root = std::env::temp_dir().join(format!("kt-cap-ssh-{}-{}", std::process::id(), tune_core::db::now_ms()));
    std::fs::create_dir_all(&root).unwrap();
    let config = root.join("config");
    std::fs::write(&config,"Host sample\n HostName example.invalid\n User fixture-user\n IdentityFile /fixture/key\n ProxyCommand true\n ControlMaster yes\n ControlPath /fixture/control\n UpdateHostKeys yes\n ClearAllForwardings no\n PermitLocalCommand yes\n LocalCommand true\n LocalForward 127.0.0.1:54321 example.invalid:9\n RemoteForward 54322 example.invalid:9\n DynamicForward 127.0.0.1:54323\n").unwrap();
    let (_, args, _) = capabilities::transport(&node("windows", false));
    let out = Command::new("ssh").arg("-G").arg("-F").arg(&config).args(args).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let option = |key: &str| text.lines().find_map(|line| line.strip_prefix(&format!("{key} "))).unwrap_or("");
    for (key, want) in [
        ("stricthostkeychecking", "true"),
        ("batchmode", "yes"),
        ("controlmaster", "false"),
        ("updatehostkeys", "false"),
        ("clearallforwardings", "yes"),
        ("permitlocalcommand", "no"),
    ] {
        assert_eq!(option(key), want, "effective {key}");
    }
    assert_eq!(option("controlpath"), "", "disabled ControlPath is omitted by ssh -G");
    assert!(!text.lines().any(|line| ["localforward ", "remoteforward ", "dynamicforward "].iter().any(|prefix| line.starts_with(prefix))));
    assert_eq!(option("hostname"), "example.invalid");
    assert_eq!(option("user"), "fixture-user");
    assert_eq!(option("identityfile"), "/fixture/key");
    assert_eq!(option("proxycommand"), "true");
    std::fs::remove_dir_all(root).unwrap();
}
