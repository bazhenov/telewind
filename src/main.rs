use clap::Parser;
use parser::parse;

mod parser;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Opts {
    /// source url to download
    #[arg(short, long, default_value_t = String::from("http://3volna.ru/anemometer/getwind?id=1"))]
    url: String,
}

#[tokio::main]
async fn main() {
    let opts = Opts::parse();
    let body = reqwest::get(opts.url).await.unwrap().text().await.unwrap();

    for observation in parse(&body) {
        println!("{observation:?}")
    }
}
