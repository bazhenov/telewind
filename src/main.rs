use parser::parse;

mod parser;

#[tokio::main]
async fn main() {
    let url = "http://3volna.ru/anemometer/getwind?id=1";

    let body = reqwest::get(url)
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    for observation in parse(&body) {
        println!("{observation:?}")
    }
}
