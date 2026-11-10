//! Conformance against `tests/fixtures/golden/egress_policy.json` (220 cases).
//!
//! - `blocked_ip?`: `parse_ip` (Ruby `IPAddr.new`) → normalized/family/blocked, or the
//!   `IPAddr::InvalidAddressError` message.
//! - `checked_target (resolve stubbed)`: a resolver that answers `resolve_returns`
//!   for any host and records the hosts it was asked for (Ruby `uri.host`).
//! - `checked_target (real resolve)` / `resolve (real)`: the system resolver with
//!   numeric hosts (glibc `inet_aton` forms). `localhost` is machine dependent
//!   (/etc/hosts order), so for it only the *set* of addresses is compared and any
//!   loopback answer is accepted for `checked_target`. macOS `getaddrinfo` reads
//!   the dotted octal `0177.0.0.1` as decimal (177.0.0.1), so off Linux that host
//!   is checked through `checked_target`, whose WHATWG re-parse must still refuse it.

use std::net::IpAddr;
use std::sync::Mutex;

use frankmd_net::NetError;
use frankmd_net::egress::{
    HostResolver, SystemResolver, Target, USERINFO_MESSAGE, check_target, checked_target,
    is_blocked_net, parse_ip,
};
use frankmd_testkit::serde_json::{Value, json};
use frankmd_testkit::{Case, assert_cases, golden};

#[derive(Debug)]
struct Stub {
    answer: Vec<IpAddr>,
    calls: Mutex<Vec<String>>,
}

impl HostResolver for Stub {
    fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, NetError> {
        self.calls.lock().expect("lock").push(host.to_owned());
        Ok(self.answer.clone())
    }
}

fn target_json(t: &Target) -> Value {
    json!({
        "uri": t.uri, "scheme": t.scheme, "host": t.host, "hostname": t.hostname,
        "port": t.port, "path": t.path, "query": t.query, "userinfo": t.userinfo,
        "fragment": t.fragment, "ip": t.ip.to_string(),
    })
}

fn result_json(r: Result<Target, NetError>) -> Value {
    match r {
        Ok(t) => target_json(&t),
        Err(NetError::Blocked(m)) => json!({"error": "EgressPolicy::BlockedError", "message": m}),
        Err(e) => json!({"error": "unexpected", "message": e.to_string()}),
    }
}

/// `true` when the URL's authority carries `userinfo@` (only the two fixture
/// cases `http://user:pass@example.com/` and `http://example.com@evil.com/`).
fn has_userinfo(url: &str) -> bool {
    url.split_once("://").is_some_and(|(_, rest)| {
        rest.split(['/', '?', '#'])
            .next()
            .is_some_and(|a| a.contains('@'))
    })
}

fn check_eq(actual: &Value, expected: &Value) -> Result<(), String> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("expected {expected}, got {actual}"))
    }
}

#[test]
fn blocked_ip() {
    let g = golden("egress_policy");
    assert_cases(
        "egress_policy blocked_ip?",
        g.cases_for("blocked_ip?"),
        |c: &Case| {
            let input = c.input["ip"].as_str().ok_or("ip not a string")?;
            let actual = match parse_ip(input) {
                // API adjustment: `parse_ip` returns `ParsedIp`, whose `Display` is Ruby's
                // `IPAddr#to_s` (keeps `%zone`, prints `::127.0.0.1` dotted), which
                // `Ipv6Addr`'s `Display` cannot reproduce.
                Ok(ip) => json!({
                    "normalized": ip.to_string(),
                    "family": if ip.addr().is_ipv4() { "ipv4" } else { "ipv6" },
                    "blocked": is_blocked_net(ip.net()),
                }),
                // The fixture recorded only the message prefix (no offending input); so must we.
                Err(NetError::InvalidAddress(m)) => {
                    json!({"error": "IPAddr::InvalidAddressError", "message": m})
                }
                Err(e) => return Err(format!("unexpected error {e:?}")),
            };
            check_eq(&actual, &c.expected)
        },
    );
}

#[test]
fn checked_target_resolve_stubbed() {
    let g = golden("egress_policy");
    assert_cases(
        "egress_policy checked_target (stubbed)",
        g.cases_for("checked_target (resolve stubbed)"),
        |c| {
            let answer = c.input["resolve_returns"]
                .as_array()
                .ok_or("resolve_returns")?
                .iter()
                .map(|v| {
                    v.as_str()
                        .unwrap_or_default()
                        .parse::<IpAddr>()
                        .map_err(|e| e.to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let stub = Stub {
                answer,
                calls: Mutex::new(Vec::new()),
            };
            // `url: null` in Ruby is `nil.to_s` == "".
            let url = c.input["url"].as_str().unwrap_or("");
            let result = result_json(check_target(url, &stub));
            let calls = json!(stub.calls.lock().expect("lock").clone());
            // security: the original accepted userinfo (`http://user:pass@example.com/`
            // passed; `http://example.com@evil.com/` was judged on evil.com). The
            // policy now refuses any userinfo before resolving; assert that instead.
            if has_userinfo(url) {
                check_eq(
                    &result,
                    &json!({"error": "EgressPolicy::BlockedError", "message": USERINFO_MESSAGE}),
                )?;
                return check_eq(&calls, &json!([]));
            }
            check_eq(&result, &c.expected["result"])?;
            check_eq(&calls, &c.expected["resolve_called_with"])
        },
    );
}

/// The one recorded host whose glibc answer other resolvers do not reproduce.
const GLIBC_OCTAL_HOST: &str = "0177.0.0.1";

#[test]
fn checked_target_real_resolve() {
    let g = golden("egress_policy");
    assert_cases(
        "egress_policy checked_target (real)",
        g.cases_for("checked_target (real resolve)"),
        |c| {
            let url = c.input["url"].as_str().ok_or("url")?;
            if cfg!(not(target_os = "linux")) && url.contains(GLIBC_OCTAL_HOST) {
                return match checked_target(url, &SystemResolver) {
                    Err(NetError::Blocked(m))
                        if m == "Refusing to fetch a non-public address (127.0.0.1)" =>
                    {
                        Ok(())
                    }
                    other => Err(format!("octal loopback not refused: {other:?}")),
                };
            }
            let result = result_json(check_target(url, &SystemResolver));
            if url.contains("localhost") {
                // Machine dependent: first answer may be 127.0.0.1 or ::1.
                let msg = result["message"].as_str().unwrap_or_default();
                return if msg == "Refusing to fetch a non-public address (::1)"
                    || msg == "Refusing to fetch a non-public address (127.0.0.1)"
                {
                    Ok(())
                } else {
                    Err(format!("localhost not refused as loopback: {result}"))
                };
            }
            check_eq(&result, &c.expected["result"])
        },
    );
}

#[test]
fn resolve_real() {
    let g = golden("egress_policy");
    assert_cases(
        "egress_policy resolve (real)",
        g.cases_for("resolve (real)"),
        |c| {
            let host = c.input["host"].as_str().ok_or("host")?;
            if cfg!(not(target_os = "linux")) && host == GLIBC_OCTAL_HOST {
                return Ok(());
            }
            let mut got: Vec<String> = SystemResolver
                .resolve(host)
                .map_err(|e| e.to_string())?
                .iter()
                .map(ToString::to_string)
                .collect();
            let mut want: Vec<String> = c
                .expected
                .as_array()
                .ok_or("expected array")?
                .iter()
                .map(|v| v.as_str().unwrap_or_default().to_owned())
                .collect();
            if host == "localhost" {
                got.sort();
                got.dedup();
                want.sort();
            }
            if got == want {
                Ok(())
            } else {
                Err(format!("expected {want:?}, got {got:?}"))
            }
        },
    );
}
