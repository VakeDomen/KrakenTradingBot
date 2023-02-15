
use krakenrs::ws::{KrakenWsConfig, KrakenWsAPI};
use krakenrs::{KrakenRestConfig, KrakenRestAPI};
use rust_decimal::prelude::ToPrimitive;
use std::{
    collections::BTreeMap,
    time::Duration,
    thread,
    env,
};
use rust_decimal::Decimal;
use dotenv::dotenv;

fn main() {
    dotenv().ok();
    let kraken_key = env::var("KRAKEN_KEY").expect("$KRAKEN_KEY empty");
    let kraken_secret = env::var("KRAKEN_SECRET").expect("$KRAKEN_SECRET empty");

    let pairs = vec!["ETH/XBT".to_string()];
    let ws_config = KrakenWsConfig {
        subscribe_book: pairs.clone(),
        book_depth: 10,
        private: None,
    };
    let ws = KrakenWsAPI::new(ws_config).expect("could not connect to websockets api");
    let kc_config = KrakenRestConfig::default();
    let api = KrakenRestAPI::try_from(kc_config).expect("could not create kraken api");

    loop {
        thread::sleep(Duration::from_millis(1000));
        // println!("{:#?}", api.ticker(vec!["BTC/ETH".to_string()]).expect("api call failed").get("BTC/USD"));
        let books = ws.get_all_books();
        for (book_id, book) in books.iter() {
            
            let asks: Vec<f64> = book.ask.keys().filter_map(|entry| entry.to_f64()).collect();
            let bids: Vec<f64> = book.bid.keys().filter_map(|entry| entry.to_f64()).collect();

            let min_ask = match asks.iter().min_by(|a, b| a.partial_cmp(b).unwrap()) {
                Some(ask) => ask,
                None => panic!(),
            };

            let max_bid = match bids.iter().min_by(|a, b| a.partial_cmp(b).unwrap()) {
                Some(bid) => bid,
                None => panic!(),
            };

            let current_price = (min_ask + max_bid) / 2.;
            println!("{}: {}", book_id, current_price);

            // for ask_dec in asks.iter() {
            //     println!("ASK: {}", ask_dec);
            // }
            // for ask_dec in bids.iter() {
            //     println!("BID: {}", ask_dec);
            // }


        }
 
        if ws.stream_closed() { return; }
    }
}