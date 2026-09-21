//! TLS: `arield` can reach an `https` prosperod (#60).
//!
//! A real certificate is not needed to test what was broken. Built without a
//! TLS backend, the client never opened a connection at all: hyper rejected the
//! URL with *invalid URL, scheme is not http*. That refusal is not in the
//! error's own message — it sits in the source chain underneath — so these
//! tests read the whole chain. With TLS built in, an `https` URL is dialled
//! like any other and a closed port fails the way `http` does.

use std::time::Duration;

use ariel_core::prospero::{ClientError, ProsperoClient};

/// Every message in an error's source chain, which is where the transport puts
/// the reason it would not dial.
fn full_chain(error: &ClientError) -> String {
    let mut messages = vec![error.to_string()];
    let mut source = std::error::Error::source(error);
    while let Some(error) = source {
        messages.push(error.to_string());
        source = error.source();
    }
    messages.join(" | ")
}

/// An address with nothing listening on it.
async fn closed_port() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    addr.to_string()
}

#[tokio::test]
async fn an_https_url_is_dialled_rather_than_refused_for_its_scheme() {
    let addr = closed_port().await;
    let client = ProsperoClient::new(&format!("https://{addr}")).unwrap();

    let error = tokio::time::timeout(Duration::from_secs(10), client.fleet())
        .await
        .expect("the client gives up on its own")
        .expect_err("nothing is listening");

    assert!(
        matches!(error, ClientError::Transport(_)),
        "an https URL must fail at the connection: {error:?}"
    );
    let chain = full_chain(&error);
    assert!(
        !chain.contains("scheme is not http"),
        "the client is still built without TLS, so it refused the URL: {chain}"
    );
}

#[tokio::test]
async fn http_and_https_fail_alike_on_a_closed_port() {
    let addr = closed_port().await;

    let plain = ProsperoClient::new(&format!("http://{addr}"))
        .unwrap()
        .fleet()
        .await
        .expect_err("nothing is listening");
    let secure = ProsperoClient::new(&format!("https://{addr}"))
        .unwrap()
        .fleet()
        .await
        .expect_err("nothing is listening");

    // Both are connection refusals, which is only true once https is dialled.
    for (scheme, error) in [("http", &plain), ("https", &secure)] {
        assert!(
            matches!(error, ClientError::Transport(_)),
            "{scheme}: {error:?}"
        );
        let chain = full_chain(error);
        assert!(
            chain.contains("Connection refused") || chain.contains("connect"),
            "{scheme} did not reach a connection attempt: {chain}"
        );
    }
}

/// The base URL is still validated, whichever scheme it carries.
#[test]
fn an_unusable_https_url_is_still_rejected() {
    assert!(ProsperoClient::new("https://").is_err());
}

/// A real TLS endpoint, which CI has no business reaching.
///
/// Run it by hand against a live prosperod:
///
/// ```text
/// ARIEL_PROSPERO_URL=https://prospero.example \
/// ARIEL_PROSPERO_TOKEN_FILE=~/.config/prospero/token \
///   cargo test -p ariel-core --test prospero_tls -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "reaches a real prosperod over the network"]
async fn a_real_https_prosperod_answers() {
    let url = std::env::var("ARIEL_PROSPERO_URL").expect("ARIEL_PROSPERO_URL");
    assert!(
        url.starts_with("https://"),
        "point this at an https URL: {url}"
    );
    let mut client = ProsperoClient::new(&url).unwrap();
    if let Ok(path) = std::env::var("ARIEL_PROSPERO_TOKEN_FILE") {
        let token = std::fs::read_to_string(&path).expect("read the token file");
        client = client.with_token(token.trim());
    }

    let fleet = client.fleet().await.expect("the fleet reads over TLS");

    println!(
        "host {}, {} workspaces over TLS",
        fleet.host,
        fleet.workspaces.len()
    );
}
