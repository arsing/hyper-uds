#[tokio::main]
async fn main() {
	let socket_path: std::borrow::Cow<'_, str> =
		if cfg!(windows) {
			format!(r"{}\hyper_uds.sock", std::env::var("TEMP").unwrap()).into()
		}
		else {
			"/tmp/hyper_uds.sock".into()
		};

	let connector = hyper_uds::UdsConnector::new();

	let client = hyper::client::Client::builder().build::<_, hyper::Body>(connector);

	request(&client, &*socket_path, "/").await;
	request(&client, &*socket_path, "/foo/?bar=baz").await;
}

async fn request(client: &hyper::client::Client<hyper_uds::UdsConnector, hyper::body::Body>, socket_path: &str, path: &str) {
	let uri = hyper_uds::make_hyper_uri(socket_path, path).unwrap();
	let response = client.get(uri).await.unwrap();
	println!("{:?}", response);
	let mut body = response.into_body();
	while let Some(data) = hyper::body::HttpBody::data(&mut body).await {
		let data = data.unwrap();
		println!("{:?}", data);
	}
}
