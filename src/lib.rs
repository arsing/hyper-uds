#![deny(rust_2018_idioms, warnings)]
#![deny(clippy::all, clippy::pedantic)]
#![allow(
	clippy::default_trait_access,
	clippy::missing_errors_doc,
	clippy::must_use_candidate,
	clippy::too_many_lines,
	clippy::type_complexity,
)]

#[cfg(windows)]
#[path = "windows.rs"]
mod sys;

#[cfg(unix)]
#[path = "unix.rs"]
mod sys;


pub type UnixStream = sys::UnixStream;

impl hyper::client::connect::Connection for UnixStream {
	fn connected(&self) -> hyper::client::connect::Connected {
		hyper::client::connect::Connected::new()
	}
}


#[derive(Clone, Debug, Default)]
pub struct UdsConnector;

impl UdsConnector {
	pub fn new() -> Self {
		Default::default()
	}
}

impl hyper::service::Service<hyper::Uri> for UdsConnector {
	type Response = UnixStream;
	type Error = std::io::Error;
	type Future = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

	fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
		std::task::Poll::Ready(Ok(()))
	}

	fn call(&mut self, req: hyper::Uri) -> Self::Future {
		Box::pin(async move {
			let scheme = req.scheme_str();
			if scheme != Some("unix") {
				return Err(std::io::Error::new(std::io::ErrorKind::Other, format!(r#"expected req to have "unix" scheme but it has {:?}"#, scheme)));
			}

			let host = req.host();

			let path: std::path::PathBuf =
				host
				.ok_or_else(|| format!("could not decode UDS path from req host {:?}", host))
				.and_then(|host| hex::decode(host).map_err(|err| format!("could not decode UDS path from req host {:?}: {}", host, err)))
				.and_then(|path| String::from_utf8(path).map_err(|err| format!("could not decode UDS path from req host {:?}: {}", host, err)))
				.map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))?
				.into();

			let stream = UnixStream::connect(path).await?;
			Ok(stream)
		})
	}
}


pub struct UdsIncoming {
	accept_stream: std::pin::Pin<Box<dyn futures_core::Stream<Item = UnixStream>>>,
}

impl hyper::server::accept::Accept for UdsIncoming {
	type Conn = UnixStream;
	type Error = std::io::Error;

	fn poll_accept(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Result<Self::Conn, Self::Error>>> {
		futures_core::Stream::poll_next(std::pin::Pin::new(&mut self.accept_stream), cx).map(|stream| stream.map(Ok))
	}
}


pub fn make_hyper_uri(base: &str, path: &str) -> Result<hyper::Uri, <hyper::Uri as std::str::FromStr>::Err> {
	let host = hex::encode(base.as_bytes());
	let uri = format!("unix://{}:0{}", host, path);
	let uri = uri.parse()?;
	Ok(uri)
}
