#[tokio::main]
async fn main() {
	let socket_path: std::borrow::Cow<'_, str> =
		if cfg!(windows) {
			format!(r"{}\hyper_uds.sock", std::env::var("TEMP").unwrap()).into()
		}
		else {
			"/tmp/hyper_uds.sock".into()
		};

	match std::fs::remove_file(&*socket_path) {
		Ok(()) => (),
		Err(ref err) if err.kind() == std::io::ErrorKind::NotFound => (),
		Err(err) => Err::<(), _>(err).unwrap(),
	}

	let incoming = hyper_uds::UdsIncoming::bind(&*socket_path).unwrap();

	let server =
		hyper::server::Server::builder(incoming)
		.serve(hyper::service::make_service_fn(|_| async {
			Ok::<_, std::io::Error>(hyper::service::service_fn(|request| async move {
				println!("{:?}", request);
				Ok::<_, std::io::Error>(hyper::Response::new(hyper::Body::from(format!("Hello from {}", request.uri()))))
			}))
		}));

	server.await.unwrap();
}
