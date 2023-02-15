
use chrono::Local;
use krakenrs::ws::{KrakenWsConfig, KrakenWsAPI, BookData};
use krakenrs::{KrakenRestConfig, KrakenRestAPI, KrakenCredentials};
use once_cell::sync::Lazy;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::collections::HashMap;
use std::sync::Mutex;
use std::{
    time::Duration,
    thread,
};


/**
 * Last executed order price
 */
pub static LAST_ORDER: Lazy<Mutex<(f64, bool)>> = Lazy::new(|| {
    match serde_any::from_file("last.data") {
        Ok(hm) => Mutex::new(hm),
        Err(e) => panic!("No price history! Can't calculate gainz!: {}", e.to_string()),
    }
});
// BTC, ETH, ETH/BTC
pub static CURRENT_PRICES: Lazy<Mutex<(Option<f64>, Option<f64>, Option<f64>)>> = Lazy::new(|| {
    Mutex::new((None, None, None))
});

pub static REST_API: Lazy<Mutex<KrakenRestAPI>> = Lazy::new(|| {
    Mutex::new(setup_rest())
});

#[derive(Debug)]
enum Position {
    Btc,
    Eth,
    None,
}

fn main() {

    let ws = setup_ws();

    {
        let last = LAST_ORDER.lock().unwrap();   
        println!("{:#?}", last);
    }
    let balance = match get_account_balance() {
        Some(balance) => balance,
        None => panic!("Couldn't get account balance!"),
    };
    for (b_key, b_val) in balance.iter() {
        println!("{}: {}", b_key, b_val);
    }

    

    loop {
        /*
         * slow down the loop
         */ 
        thread::sleep(Duration::from_millis(1000));

        /*
         * update prices on tick
         */
        update_prices(ws.get_all_books());

        /*
         * get my balance and my position
         */
        let balance = match get_account_balance() {
            Some(bal) => bal,
            None => panic!("[{} | LOOP] Error getting account balance", time()),
        };
        let position = get_my_position(&balance);
        println!("POSITION: {:#?}", position);

        /*
         * get current gain
         */
        let gain = calculate_gain(position);
        println!("GAIN: {}", gain);


        // print_prices();

        /*
         * stop loop on stream closed 
         */
        if ws.stream_closed() { return; }
    }
}

fn calculate_gain(position: Position) -> f64 {
    
}

fn print_prices() {
    let prices = CURRENT_PRICES.lock().unwrap();
    println!("****** PRICES ******");
    println!("ETH/XBT: {:#?}", prices.2.unwrap_or(0.));
    println!("ETH: {:#?}€", prices.1.unwrap_or(0.));
    println!("BTC: {:#?}€", prices.0.unwrap_or(0.));
    println!("********************");
}

fn update_prices(books: std::collections::BTreeMap<String, BookData>) {
    for (book_id, book) in books.iter() {
        let (bids, asks) = extract_bids_and_asks_from_book(book);
        
        let min_ask = match asks.iter().min_by(|a, b| a.partial_cmp(b).unwrap()) {
            Some(ask) => ask,
            None => panic!(),
        };

        let max_bid = match bids.iter().max_by(|a, b| a.partial_cmp(b).unwrap()) {
            Some(bid) => bid,
            None => panic!(),
        };
        println!("{}", book_id);
        let current_price = (min_ask + max_bid) / 2.;
        update_price(book_id, current_price);
    }
}

fn update_price(book_id: &str, current_price: f64) {
    let mut prices = CURRENT_PRICES.lock().unwrap();
    if book_id.eq("ETH/XBT") {
        prices.2 = Some(current_price);
    }
    if book_id.eq("ETH/EUR") {
        prices.1 = Some(current_price);
    }
    if book_id.eq("XBT/EUR") {
        prices.0 = Some(current_price);
    }
}

fn get_account_balance() -> Option<HashMap<String, Decimal>> {
    let rest = REST_API.lock().unwrap();
    match rest.get_account_balance() {
        Ok(bal) => Some(bal),
        Err(e) => {
            println!("[{} | GET ACCOUTN BALANCE] Error: {:#?}", time(), e);    
            None
        }
    }
}

fn extract_bids_and_asks_from_book(book: &BookData) -> (Vec<f64>, Vec<f64>) {
    let asks: Vec<f64> = book.ask.keys().filter_map(|entry| entry.to_f64()).collect();
    let bids: Vec<f64> = book.bid.keys().filter_map(|entry| entry.to_f64()).collect();
    (bids, asks)
}

fn setup_ws() -> KrakenWsAPI {
    let pairs = vec![
        "ETH/XBT".to_string(), 
        "ETH/EUR".to_string(), 
        "XBT/EUR".to_string()
    ];
    let ws_config = KrakenWsConfig {
        subscribe_book: pairs.clone(),
        book_depth: 10,
        private: None,
    };
    KrakenWsAPI::new(ws_config).expect("could not connect to websockets api")
}

fn setup_rest() -> KrakenRestAPI {
    let creds = match KrakenCredentials::load_json_file("./creds.json") {
        Ok(creds) => creds,
        Err(e) => panic!("{}", e.to_string()),
    };
    let mut kraken_config = KrakenRestConfig::default();
    kraken_config.creds = creds;
    KrakenRestAPI::try_from(kraken_config).expect("could not create kraken rest api")
}

fn save_last_order() {
    let mut last = LAST_ORDER.lock().unwrap();
    match serde_any::to_file("last.data", &*last) {
        Ok(_) => {();},
        Err(e) => {println!("[{} | ORDER SAVE] Error saving last.data: {:#?}", time(), e);}
    };
}

fn get_my_position(balance: &HashMap<String, Decimal>) -> Position {
    let btc_position = match balance.get("XXBT") {
        Some(pos) => pos.to_f64().unwrap(),
        None => 0.,
    };

    let eth_position = match balance.get("XETH") {
        Some(pos) => pos.to_f64().unwrap(),
        None => 0.,
    };

    let btc_eur = get_btc_value(btc_position);
    let eth_eur = get_eth_value(eth_position);

    if btc_eur == 0. && eth_eur == 0. {
        Position::None
    } else if btc_eur > eth_eur {
        Position::Btc
    } else {
        Position::Eth
    }
}

fn get_btc_value(ammount: f64) -> f64 {
    let val = {
        let prices = CURRENT_PRICES.lock().unwrap();
        match prices.0 {
            Some(pr) => pr,
            None => panic!("[{} | PRICE CALC] Can't find BTC price!", time()),
        }
    };
    ammount * val
}

fn get_eth_value(ammount: f64) -> f64 {
    let val = {
        let prices = CURRENT_PRICES.lock().unwrap();
        match prices.1 {
            Some(pr) => pr,
            None => panic!("[{} | PRICE CALC] Can't find ETH price!", time()),
        }
    };
    ammount * val
}

fn time() -> String {
    Local::now().format("%d-%m-%Y %H:%M:%S").to_string()
}

