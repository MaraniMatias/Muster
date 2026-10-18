//! Discovery of the OpenCode service a person starts themselves.
//!
//! Muster is a client of that service and never its owner. The service registers itself so its
//! clients can find it, this reads that registration and checks that the service answers there.
//! Nothing here starts a server and nothing here ends one: a service that is not running leaves
//! the app disconnected, which is a state to wait in, not a failure to clean up after.

use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use base64::Engine as _;
use serde::Deserialize;

/// Where the service registers itself, and the name it registers under.
const STATE_DIR_ENV: &str = "XDG_STATE_HOME";
const STATE_DIR_FROM_HOME: &str = ".local/state";
const SERVICE_DIR: &str = "opencode";
const SERVICE_FILE: &str = "service.json";

/// The API major this app speaks. A service on another major answers a different contract, and
/// Muster would be sending a request shaped for this one against it.
const SUPPORTED_MAJOR: &str = "2";

/// A discovery probe is a single loopback request. A service that is gone fails to connect at
/// once, and one that is wedged is not worth holding the UI for long.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// What a person sees when there is no service to talk to.
pub const NOT_RUNNING: &str = "OpenCode is not running. Start it and this app connects to it.";

/// A running service, as its own registration describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEndpoint {
    /// The origin every request goes to, without a trailing slash.
    pub url: String,
    /// That origin's port, which the event stream opens as a raw socket.
    pub port: u16,
    /// The password the registration carries, which every request presents.
    pub password: String,
}

/// What the service writes about itself. The rest of the file is its business, not this app's.
#[derive(Debug, Deserialize)]
struct Registration {
    url: String,
    password: String,
    version: String,
}

/// Where the service may have registered itself, most specific first.
pub fn registration_files(home: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(state) = std::env::var_os(STATE_DIR_ENV).filter(|state| !state.is_empty()) {
        files.push(PathBuf::from(state).join(SERVICE_DIR).join(SERVICE_FILE));
    }
    files.push(
        home.join(STATE_DIR_FROM_HOME)
            .join(SERVICE_DIR)
            .join(SERVICE_FILE),
    );
    files
}

/// Finds the running service, or says why there is not one.
///
/// A registration that outlives its service is the normal way this fails: the file is written
/// once at startup and is still there after the service is gone. The probe is what tells those
/// two apart, which is why reading the file is not treated as finding a service.
pub fn discover(home: &Path) -> Result<ServiceEndpoint, String> {
    let registration = read_registration(home)?;
    supported_version(&registration.version)?;
    let endpoint = endpoint(&registration.url, registration.password)?;
    probe(&endpoint)?;
    Ok(endpoint)
}

/// The authorization header value the service expects. It is basic auth over a fixed username,
/// because the registration carries the password and nothing else identifies a client.
pub fn authorization(password: &str) -> String {
    let credentials = format!("opencode:{password}");
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(credentials)
    )
}

fn read_registration(home: &Path) -> Result<Registration, String> {
    let mut found = None;
    for file in registration_files(home) {
        match fs::read_to_string(&file) {
            Ok(contents) => {
                found = Some(contents);
                break;
            }
            Err(_) => continue,
        }
    }
    let contents = found.ok_or_else(|| NOT_RUNNING.to_string())?;
    serde_json::from_str(&contents)
        .map_err(|_| "the OpenCode service registration could not be read".into())
}

fn supported_version(version: &str) -> Result<(), String> {
    match version.split('.').next() {
        Some(major) if major == SUPPORTED_MAJOR => Ok(()),
        _ => Err(format!(
            "the running OpenCode service is version {version}, and this app speaks version \
             {SUPPORTED_MAJOR}"
        )),
    }
}

/// Reads the origin out of the registration and keeps it only if it is one this app may send the
/// password to.
///
/// The service binds loopback, so an address that is not loopback is either a different machine
/// or a rewritten file, and either way the registration's password does not belong on the wire
/// to it. Plain HTTP is likewise what the service serves, so a scheme this client has no TLS
/// support for is refused rather than downgraded.
fn endpoint(url: &str, password: String) -> Result<ServiceEndpoint, String> {
    let refused = || format!("the OpenCode service registered an address Muster cannot use: {url}");
    let authority = url
        .strip_prefix("http://")
        .ok_or_else(refused)?
        .split('/')
        .next()
        .unwrap_or_default();
    let address: SocketAddr = authority.parse().map_err(|_| refused())?;
    if !address.ip().is_loopback() {
        return Err(refused());
    }
    Ok(ServiceEndpoint {
        url: format!("http://{authority}"),
        port: address.port(),
        password,
    })
}

/// Asks the service whether it is there, with the password that says this client may ask.
fn probe(endpoint: &ServiceEndpoint) -> Result<(), String> {
    let response = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(PROBE_TIMEOUT))
            .timeout_connect(Some(PROBE_TIMEOUT))
            .build(),
    )
    .get(&format!("{}/api/info", endpoint.url))
    .header("authorization", authorization(&endpoint.password))
    .call();
    match response {
        Ok(_) => Ok(()),
        Err(ureq::Error::StatusCode(status)) => Err(format!(
            "the running OpenCode service refused this app with status {status}"
        )),
        Err(_) => Err(NOT_RUNNING.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn home_with_registration(contents: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join(STATE_DIR_FROM_HOME).join(SERVICE_DIR);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(SERVICE_FILE), contents).unwrap();
        home
    }

    fn registration_for(port: u16, password: &str, version: &str) -> String {
        serde_json::json!({
            "id": "9360a1e9",
            "version": version,
            "url": format!("http://127.0.0.1:{port}"),
            "pid": 4242,
            "password": password,
        })
        .to_string()
    }

    /// A service that answers `/api/info` only to the password in its registration.
    fn serving(password: &'static str) -> (u16, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            use std::io::{Read, Write};
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = [0_u8; 1024];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = &buffer[..read];
            // `authorization` is the whole header value, prefix included.
            let expected = authorization(password);
            let status = if String::from_utf8_lossy(request).contains(&expected) {
                "200 OK"
            } else {
                "401 Unauthorized"
            };
            let _ = stream.write_all(
                format!("HTTP/1.1 {status}\r\ncontent-length: 2\r\n\r\n{{}}").as_bytes(),
            );
        });
        (port, handle)
    }

    #[test]
    fn a_service_that_answers_is_found_without_anything_being_started() {
        let (port, server) = serving("secret");
        let home = home_with_registration(&registration_for(port, "secret", "2.0.22"));

        let endpoint = discover(home.path()).unwrap();

        assert_eq!(endpoint.port, port);
        assert_eq!(endpoint.url, format!("http://127.0.0.1:{port}"));
        assert_eq!(endpoint.password, "secret");
        server.join().unwrap();
    }

    #[test]
    fn nobody_running_reads_as_absent_rather_than_as_a_failure() {
        let home = tempfile::tempdir().unwrap();

        // The registration a running service would have written is not there, which is the state
        // this app waits in until the user starts OpenCode.
        assert_eq!(discover(home.path()).unwrap_err(), NOT_RUNNING);
    }

    #[test]
    fn a_registration_whose_service_is_gone_reads_as_absent() {
        // The file outlives the process that wrote it, so a stale registration is the ordinary
        // case and must not be reported as a running service.
        let taken = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        drop(taken);
        let home = home_with_registration(&registration_for(port, "secret", "2.0.22"));

        assert_eq!(discover(home.path()).unwrap_err(), NOT_RUNNING);
    }

    #[test]
    fn a_wrong_password_is_reported_as_a_refusal_rather_than_as_absent() {
        let (port, server) = serving("the-real-one");
        let home = home_with_registration(&registration_for(port, "a-stale-one", "2.0.22"));

        let error = discover(home.path()).unwrap_err();

        // A service that is there but will not have this app is a different problem from one that
        // is not running, and the difference is what tells a person whether to look at the file.
        assert!(error.contains("401"), "{error}");
        server.join().unwrap();
    }

    #[test]
    fn another_api_major_is_refused_before_the_password_is_sent() {
        let home = home_with_registration(&registration_for(45999, "secret", "1.9.0"));

        let error = discover(home.path()).unwrap_err();

        assert!(error.contains("version 1.9.0"), "{error}");
    }

    #[test]
    fn an_address_off_the_machine_is_refused_because_the_password_is_not_sent_to_it() {
        let home = home_with_registration(
            &serde_json::json!({
                "version": "2.0.22",
                "url": "http://192.168.1.10:4096",
                "password": "secret",
            })
            .to_string(),
        );

        let error = discover(home.path()).unwrap_err();

        assert!(error.contains("cannot use"), "{error}");
    }

    #[test]
    fn the_registration_path_follows_the_state_directory_when_one_is_set() {
        let home = tempfile::tempdir().unwrap();
        let previous = std::env::var_os(STATE_DIR_ENV);
        // The variable is read from the process environment, so this asserts against a path this
        // test controls rather than against whatever the machine running it happens to use.
        unsafe { std::env::set_var(STATE_DIR_ENV, home.path().join("state")) };
        let files = registration_files(home.path());
        match previous {
            Some(value) => unsafe { std::env::set_var(STATE_DIR_ENV, value) },
            None => unsafe { std::env::remove_var(STATE_DIR_ENV) },
        }

        assert!(
            files[0].ends_with(Path::new("state").join(SERVICE_DIR).join(SERVICE_FILE)),
            "{files:?}"
        );
        assert!(
            files[1].ends_with(
                Path::new(STATE_DIR_FROM_HOME)
                    .join(SERVICE_DIR)
                    .join(SERVICE_FILE)
            ),
            "{files:?}"
        );
    }

    #[test]
    fn the_authorization_header_is_the_password_over_a_fixed_username() {
        // Basic `opencode:secret`, which is what the service registration is for.
        assert_eq!(
            authorization("secret"),
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("opencode:secret")
            )
        );
    }
}
