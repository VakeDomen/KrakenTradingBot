
use chrono::Local;
use krakenrs::ws::{KrakenWsConfig, KrakenWsAPI, BookData};
use krakenrs::{KrakenRestConfig, KrakenRestAPI, KrakenCredentials, BsType, LimitOrder, AddOrderResponse, GetOpenOrdersResponse, CancelAllOrdersResponse};
use once_cell::sync::Lazy;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use teloxide::payloads::SendMessageSetters;
use tokio::runtime::Handle;
use tokio::task::block_in_place;
use std::collections::{HashMap, BTreeSet};
use std::io::{Error, ErrorKind};
use std::sync::Mutex;
use std::{
    time::Duration,
    thread,
    env
};
use dotenv::dotenv;
use teloxide::Bot;
use teloxide::dispatching::repls::CommandReplExt;
use teloxide::requests::Requester;
use teloxide::requests::ResponseResult;
use teloxide::types::{Message, ChatId, ParseMode};
use teloxide::utils::command::BotCommands;

/**
 * Last executed order price
 */
pub static LAST_ORDER: Lazy<Mutex<(f64, bool, )>> = Lazy::new(|| {
    match serde_any::from_file("last.json") {
        Ok(hm) => Mutex::new(hm),
        Err(e) => panic!("No price history! Can't calculate gainz!: {}", e.to_string()),
    }
});
pub static LAST_COMPLETED_ORDER: Lazy<Mutex<(f64, bool, )>> = Lazy::new(|| {
    match serde_any::from_file("last_completed.json") {
        Ok(hm) => Mutex::new(hm),
        Err(e) => panic!("No price history of last completed order!: {}", e.to_string()),
    }
});
// BTC, ETH, ETH/BTC
pub static CURRENT_PRICES: Lazy<Mutex<(Option<f64>, Option<f64>, Option<f64>)>> = Lazy::new(|| {
    Mutex::new((None, None, None))
});

pub static REST_API: Lazy<Mutex<KrakenRestAPI>> = Lazy::new(|| {
    Mutex::new(block_in_place(|| setup_rest()))
});

#[derive(Debug)]
enum Position {
    Btc,
    Eth,
    None,
}

#[tokio::main]
async fn main() {
    check_env();
    let _thread_handle = thread::spawn(|| { run_bot(); });
    let ws = block_in_place(|| setup_ws());

    let mut balance = match get_account_balance() {
        Some(balance) => balance,
        None => panic!("Couldn't get account balance!"),
    };
    let mut balance_stained = false;
    let mut time_to_wait_in_millis = 5000;

    /*
     * reconnection loop 
     */
    loop {
        /*
        * main loop
        */
        loop {
            /*
            * slow down the loop
            */ 
            thread::sleep(Duration::from_millis(time_to_wait_in_millis));
            /*
            *  check order resolution update 
            *  NOTE: must be above balance update, so it 
            *  updates after order resolution
            */
            if is_waiting_order_resolution() {
                time_to_wait_in_millis = 30000;
                let orders = get_open_orders();
                match orders {
                    Ok(ord) => {
                        if ord.open.len() == 0 {
                            complete_last_order();
                            time_to_wait_in_millis = 5000;
                            notify_order_completed_telegram();
                        }
                    },
                    Err(e) => { println!("[{} | ORDER RESOLUTION WAIT] Could not fetch open orders: {}", time(), e.to_string())},
                }
                println!("[{} | ORDER RESOLUTION WAIT] Waiting for order to resolve", time());
                continue;
            }

            /*
            * update prices on tick
            */
            update_prices(block_in_place(|| ws.get_all_books()));

            /*
            * get my balance
            */
            if balance_stained {
                balance = match get_account_balance() {
                    Some(bal) => bal,
                    None => panic!("[{} | LOOP] Error getting account balance", time()),
                };
                balance_stained = false;
            }

            /*
            * calc my position from balance 
            */
            let position = get_my_position(&balance);

            /*
            * get current gain
            */
            let gain = match calculate_gain(&position) {
                Some(gain) => gain,
                None => panic!("[{} | LOOP] Error calculating gain!", time()),
            };
            println!("GAIN: {:#?}", gain);

            /*
            * hop strat eval
            */
            let should_hop = match position {
                Position::Btc => gain > 0.02,
                Position::Eth => gain > 0.05,
                Position::None => false,
            };

            /*
            * execute hop
            */
            if should_hop {
                match execute_hop(&position, &balance) {
                    Ok((order_response, price)) => {
                        println!("[{} | EXECUTED TRADE] Order placed: {:#?}", time(), order_response); 
                        update_last_order(price);
                        balance_stained = true;
                        notify_order_placed_telegram(&order_response, &price, &gain, &position);
                    },
                    Err(e) => panic!("PANIC: {}", e.to_string())
                };
            }

            /*
            * stop loop on stream closed 
            */
            if ws.stream_closed() { 
                println!("Stream closed");
                notify_stream_close();
                break; 
            }
        }
        thread::sleep(Duration::from_secs(10))
    }
}



fn check_env() {
    dotenv().ok();
    let _report_chat = env::var("TELEGRAM_REPORT_CHAT_ID").expect("$TELEGRAM_REPORT_CHAT_ID is not set").parse::<i64>().unwrap();
    let token = env::var("TELEGRAM_BOT_TOKEN").expect("$TELEGRAM_BOT_TOKEN is not set");
    env::set_var("TELOXIDE_TOKEN", token);
}

fn update_last_order(price: f64) {
    {
        let mut last = LAST_ORDER.lock().unwrap();
        last.0 = price;
        last.1 = false;
    }
    save_last_order();
}

fn complete_last_order() {
    {
        let mut last = LAST_ORDER.lock().unwrap();
        let mut last_completed = LAST_COMPLETED_ORDER.lock().unwrap();
        last.1 = true;
        last_completed.0 = last.0;
        last_completed.1 = last.1;
    }
    save_last_order();
    save_last_completed_order();
}

fn get_open_orders() -> Result<GetOpenOrdersResponse, krakenrs::Error> {
    let api = REST_API.lock().unwrap();
    block_in_place(|| api.get_open_orders(None))
}

fn is_waiting_order_resolution() -> bool {
    let last = LAST_ORDER.lock().unwrap();
    !last.1
}

fn converted_volume(position: &Position, volume: f64) -> f64 {
    match position {
        Position::Btc => volume / get_currnet_relative_price().unwrap(),
        Position::Eth => volume,
        Position::None => 0.,
    }
}

fn execute_hop(position: &Position, balance: &HashMap<String, Decimal>) -> Result<(AddOrderResponse, f64), Error> {
    let bs_type = match position {
        Position::Btc => BsType::Buy,
        Position::Eth => BsType::Sell,
        Position::None => return Err(Error::new(
            ErrorKind::Other, 
            format!("[{} | EXECUTE HOP] No position specified!", time())
        )),
    };

    let volume_option = match position {
        Position::Btc => balance.get("XXBT"),
        Position::Eth => balance.get("XETH"),
        Position::None => return Err(Error::new(
            ErrorKind::Other, 
            format!("[{} | EXECUTE HOP] No position specified!", time())
        )),
    };

    let base_volume = match volume_option {
        Some(vol) => vol.to_f64().unwrap(),
        None => return Err(Error::new(
            ErrorKind::Other, 
            format!("[{} | EXECUTE HOP] Error geting volume from balance!", time())
        )),
    };

    let volume = converted_volume(position, base_volume).to_string();

    let pair = "ETH/BTC".to_string();

    let price_float = match get_currnet_relative_price() {
        Some(pr) => round_to_precision(pr),
        None => return Err(Error::new(
            ErrorKind::Other, 
            format!("[{} | EXECUTE HOP] Error geting price from state!", time())
        )),
    };
    let price = price_float.to_string();

    let oflags = BTreeSet::new();

    let api = REST_API.lock().unwrap();
    let limit_order = LimitOrder {
        bs_type,
        volume,
        pair,
        price,
        oflags,
    };
    block_in_place(|| match api.add_limit_order(
        limit_order, 
        None, 
        false
    ) {
        Ok(r) => Ok((r, price_float)),
        Err(e) => Err(Error::new(
            ErrorKind::Other, 
            format!("[{} | EXECUTE HOP] Error executing transaction: {}", time(), e.to_string())
        )),
    })
}

fn round_to_precision(num: f64) -> f64 {
    (num * 100000.0).round() / 100000.0
}

fn calculate_gain(position: &Position) -> Option<f64> {
    let (last_value, last_completed) = get_last_trade();
    if !last_completed {
        return None;
    }

    if let Position::None = position {
        return None;
    }

    let current_value = match get_currnet_relative_price() {
        Some(val) => val,
        None => return None,
    };

    let gain_ratio = (current_value / last_value) - 1.;

    match position {
        Position::Btc => Some(gain_ratio * -1.),
        Position::Eth => Some(gain_ratio),
        Position::None => return None,
    }
}

fn get_currnet_relative_price() -> Option<f64> {
    let prices = CURRENT_PRICES.lock().unwrap();
    prices.2
}

fn get_currnet_eth_price() -> Option<f64> {
    let prices = CURRENT_PRICES.lock().unwrap();
    prices.1
}

fn get_currnet_btc_price() -> Option<f64> {
    let prices = CURRENT_PRICES.lock().unwrap();
    prices.0
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
    block_in_place(|| match rest.get_account_balance() {
        Ok(bal) => Some(bal),
        Err(e) => {
            println!("[{} | GET ACCOUTN BALANCE] Error: {:#?}", time(), e);    
            None
        }
    })
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
    let last = LAST_ORDER.lock().unwrap();
    match serde_any::to_file("last.json", &*last) {
        Ok(_) => {();},
        Err(e) => {println!("[{} | ORDER SAVE] Error saving last.json: {:#?}", time(), e);}
    };
}

fn save_last_completed_order() {
    let last = LAST_COMPLETED_ORDER.lock().unwrap();
    match serde_any::to_file("last_completed.json", &*last) {
        Ok(_) => {();},
        Err(e) => {println!("[{} | ORDER SAVE] Error saving last_completed.json: {:#?}", time(), e);}
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

    let btc_eur = get_btc_value(btc_position).unwrap_or(0.);
    let eth_eur = get_eth_value(eth_position).unwrap_or(0.);

    if btc_eur == 0. && eth_eur == 0. {
        Position::None
    } else if btc_eur > eth_eur {
        Position::Btc
    } else {
        Position::Eth
    }
}

fn get_last_trade() -> (f64, bool) {
    let last = LAST_ORDER.lock().unwrap();
    (last.0, last.1)
}

fn load_previous_order() {
    {
        let mut last = LAST_ORDER.lock().unwrap();
        let last_completed = LAST_COMPLETED_ORDER.lock().unwrap();
        last.0 = last_completed.0;
        last.1 = last_completed.1;
    }
    save_last_completed_order();
}

fn get_btc_value(ammount: f64) -> Option<f64> {
    let val = {
        let prices = CURRENT_PRICES.lock().unwrap();
        match prices.0 {
            Some(pr) => pr,
            None => return None,
        }
    };
    Some(ammount * val)
}

fn get_eth_value(ammount: f64) -> Option<f64> {
    let val = {
        let prices = CURRENT_PRICES.lock().unwrap();
        match prices.1 {
            Some(pr) => pr,
            None => return None,
        }
    };
    Some(ammount * val)
}

fn cancel_all_orders() -> Result<CancelAllOrdersResponse, krakenrs::Error> {
    let api = REST_API.lock().unwrap();
    block_in_place(|| api.cancel_all_orders())
}

fn time() -> String {
    Local::now().format("%d-%m-%Y %H:%M:%S").to_string()
}

#[tokio::main]
async fn run_bot() {
    let bot = Bot::from_env();
    Command::repl(bot, answer).await;
}


#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "These commands are supported:")]
enum Command {
    #[command(description = "display this text.")]
    Id,
    Balance,
    Price,
    Abort,
}


async fn answer(
    bot: Bot,
    message: Message,
    command: Command,
) -> ResponseResult<()> {

    match command {
        Command::Id => bot.send_message(get_report_chat_id(), parse_id(message)).await? ,
        Command::Balance => bot.send_message(get_report_chat_id(), generate_balance_string()).parse_mode(ParseMode::MarkdownV2).await?,
        Command::Price => bot.send_message(get_report_chat_id(), generate_price_string()).parse_mode(ParseMode::MarkdownV2).await?,
        Command::Abort => bot.send_message(get_report_chat_id(), abort_order()).parse_mode(ParseMode::MarkdownV2).await?,
    };
    Ok(())
}

fn abort_order() -> String {
    match cancel_all_orders() {
        Ok(c) => (),
        Err(e) => return format!("```Could not cancel order: {:#?}```", e),
    };
    load_previous_order();
    "Successfully reverted order!".to_string()
}

fn generate_price_string() -> String {
    let balance_handle = thread::spawn(|| {
        get_account_balance()
    });
    
    let balance = match balance_handle.join() {
        Ok(b_option) => match b_option {
            Some(b) => b,
            None => return "Could not fetch balance".to_string(),
        },
        Err(e) => return format!("{:#?}", e),
    };

    let position = get_my_position(&balance);
    let gain = match calculate_gain(&position) {
        Some(gain) => gain,
        None => return format!("Error calculating gain!"),
    };

    let icon = if gain > 0. {
        "🟢"
    } else {
        "🔴"
    };

    format!(
        "```\nRELATIVE: {:.5}\nXXBT:\t\t{:.2}\nXETH:\t\t{:.2}\nGAIN:\t\t{:.2}% {}\n```",
        get_currnet_relative_price().unwrap(),
        get_currnet_btc_price().unwrap(),
        get_currnet_eth_price().unwrap(),
        (gain * 100.),
        icon
    )    
}

fn generate_balance_string() -> String {
    let balance_option = get_account_balance();
    
    let balance = match balance_option {
        Some(b) => b,
        None => return "Could not fetch balance".to_string(),
    };

    let mut out = "```".to_string();
    for (b_key, b_val) in balance.iter() {
        let f_val = b_val.to_f64().unwrap_or(0.);
        let eur_val = if b_key.eq("XETH") {
            get_eth_value(f_val).unwrap_or(0.)
        } else if b_key.eq("XXBT") {
            get_btc_value(f_val).unwrap_or(0.)
        } else {
            0.
        };
        let mut keyclone = b_key.clone();
        keyclone.push_str("     ");
        out = format!("{}\n{:.5}:  \t{:.2}€ \t({:.4})", out, keyclone, eur_val, b_val);
    }
    format!("{}\n```", out)
}

fn parse_id(message: Message) -> String {
    format!("Your chat id: {:#?}", message.chat.id)
}

fn get_report_chat_id() -> ChatId {
    let report_chat = env::var("TELEGRAM_REPORT_CHAT_ID")
        .expect("$TELEGRAM_REPORT_CHAT_ID is not set")
        .parse::<i64>()
        .unwrap();
    ChatId(report_chat)
}

fn notify_order_placed_telegram(order_response: &AddOrderResponse, price: &f64, gain: &f64, position: &Position) {
    let balance_option = get_account_balance();
    
    let balance = match balance_option {
        Some(b) => b,
        None => return,
    };
    
    let other_postition = match position {
        Position::Btc => Position::Eth,
        Position::Eth => Position::Btc,
        Position::None => Position::None,
    };
    let volume_option = match position {
        Position::Btc => balance.get("XXBT"),
        Position::Eth => balance.get("XETH"),
        Position::None => return,
    };

    let base_volume = match volume_option {
        Some(vol) => vol.to_f64().unwrap(),
        None => return,
    };

    let volume = converted_volume(position, base_volume);

    send_telegram_message(format!(
        "Placed an order: 💰\n```\n{:?}\n```\nSummary: 📂\n```\nPRICE: {}\nGAIN: {:.3}%\nPOSITION: {:?} -> {:?}\nVOLUME: {:.5}\n```", 
        order_response.descr.order,
        price, 
        (gain * 100.),
        position,
        other_postition,
        volume
    ));
}

fn notify_order_completed_telegram() {
    send_telegram_message(format!("Last order seems to have been filled 🎉💰"));
}


pub fn send_telegram_message(message: String) {
    let report_chat = env::var("TELEGRAM_REPORT_CHAT_ID")
        .expect("$TELEGRAM_REPORT_CHAT_ID is not set")
        .parse::<i64>()
        .unwrap();

    block_in_place( move || {
        Handle::current().block_on(async move {
            match Bot::from_env().send_message(
                ChatId(report_chat),
                message
            ).parse_mode(ParseMode::MarkdownV2).await {
                Ok(e) => println!("Sent message: {:?}", e),
                Err(e) => println!("Error sending message {:#?}", e),
            }
        });
    });
}

fn notify_stream_close() {
    send_telegram_message(format!("Stream closed from Kraken! Will try to reconnect in 5mins."));
}