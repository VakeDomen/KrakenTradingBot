# KrakenTradingBot
Rust based crypto trading bot. Hops between BTC and ETH based on the BTC/ETH ratio. Requires [Kraken](kraken.com) account and `creds.json` obtained from Kraken. Also requires a `.env` file with 2 set variables for reporting via telegram:
```
TELEGRAM_BOT_TOKEN=
TELEGRAM_REPORT_CHAT_ID=
```

`TELEGRAM_BOT_TOKEN` corresponds to the token created by telegram BotFather and enables bot reporting via Telegram. `TELEGRAM_REPORT_CHAT_ID` is the id of the chat to report to (your private chat id). 

## Building and running
Build with:
```
cargo build --release
```

and run with:
```
./run.sh
```
