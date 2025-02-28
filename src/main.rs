mod protos;
use rdkafka::config::ClientConfig;
use rdkafka::consumer::{CommitMode, Consumer, StreamConsumer};
use rdkafka::message::Message;
use rdkafka::error::KafkaResult;
use tokio::time::{timeout, Duration};
use prost::Message as ProstMessage;
use crate::protos::dex_block_message::DexParsedBlockMessage;
use crate::protos::dex_block_message::*;

#[tokio::main]
async fn main() -> KafkaResult<()> {
    // Optional: initialize logger
    env_logger::init();

    // Create a `StreamConsumer` to read messages asynchronously
    let consumer: StreamConsumer = ClientConfig::new()
        // Multiple bootstrap servers
        .set("bootstrap.servers", "rpk0.bitquery.io:9093,rpk1.bitquery.io:9093,rpk2.bitquery.io:909")
        .set("security.protocol", "SASL_SSL")
        .set("ssl.certificate.location", "client.cer.pem")
        .set("ssl.key.location", "client.key.pem")
        .set("ssl.key.password", "")
        .set("ssl.ca.location", "server.cer.pem")
        .set("ssl.endpoint.identification.algorithm", "none")
        .set("sasl.mechanisms", "SCRAM-SHA-512")
        .set("sasl.username", "USER")
        .set("sasl.password", "PASS")
        .set("group.id", "GROUPID")
        .set("fetch.message.max.bytes", "10485760") // 10 MB
        .create()?;

    // Subscribe to your Bitquery topic(s)
    let topics = vec!["solana.dextrades.proto"];
    consumer.subscribe(&topics)?;

    println!("Starting consumer, listening on topics: {:?}", topics);

    // We'll store the latest known SOL price in USDC here
    let mut last_sol_price_in_usdc: Option<f64> = None;
    let known_usdc_symbol = "USDC";
    let known_sol_symbol = "SOL";

    loop {
        // Attempt to receive a message with a 10 second timeout
        match timeout(Duration::from_secs(10), consumer.recv()).await {
            Ok(msg_result) => match msg_result {
                Ok(msg) => {
                    if let Some(payload) = msg.payload() {
                        // Try to decode the protobuf message
                        match DexParsedBlockMessage::decode(payload) {
                            Ok(parsed_block) => {
                                for dex_tx in &parsed_block.transactions {
                                    if let Some(status) = &dex_tx.status {
                                        if status.success {
                                            for trade in &dex_tx.trades {
                                                if let (Some(buy_side), Some(sell_side)) = (&trade.buy, &trade.sell) {
                                                    let buy_currency = match &buy_side.currency {
                                                        Some(cur) => cur,
                                                        None => continue,
                                                    };
                                                    let sell_currency = match &sell_side.currency {
                                                        Some(cur) => cur,
                                                        None => continue,
                                                    };

                                                    let buy_amount = buy_side.amount as f64
                                                        / 10f64.powi(buy_currency.decimals as i32);
                                                    let sell_amount = sell_side.amount as f64
                                                        / 10f64.powi(sell_currency.decimals as i32);

                                                    // Skip if amounts are zero
                                                    if buy_amount == 0.0 || sell_amount == 0.0 {
                                                        continue;
                                                    }

                                                    let buy_symbol = &buy_currency.symbol;
                                                    let sell_symbol = &sell_currency.symbol;

                                                    // If either side is USDC, do direct calculation
                                                    let buy_is_usdc = buy_symbol == known_usdc_symbol;
                                                    let sell_is_usdc = sell_symbol == known_usdc_symbol;

                                                    // If either side is SOL, do bridging logic if we have sol_usdc
                                                    let buy_is_sol = buy_symbol == known_sol_symbol;
                                                    let sell_is_sol = sell_symbol == known_sol_symbol;

                                                    // Direct USDC pair
                                                    if buy_is_usdc || sell_is_usdc {
                                                        let token_price_in_usdc = if buy_is_usdc {
                                                            buy_amount / sell_amount
                                                        } else {
                                                            sell_amount / buy_amount
                                                        };

                                                        let token_name = if buy_is_usdc {
                                                            sell_symbol
                                                        } else {
                                                            buy_symbol
                                                        };

                                                        println!("Token {} price in USDC: {}", token_name, token_price_in_usdc);

                                                        // If the pair is SOL <-> USDC, update last_sol_price_in_usdc
                                                        if (buy_is_sol && sell_is_usdc) || (sell_is_sol && buy_is_usdc) {
                                                            last_sol_price_in_usdc = Some(
                                                                if buy_is_sol {
                                                                    // buy_amount is SOL, sell_amount is USDC => 1 SOL = USDC
                                                                    sell_amount / buy_amount
                                                                } else {
                                                                    // sell_amount is SOL, buy_amount is USDC => 1 SOL = USDC
                                                                    buy_amount / sell_amount
                                                                }
                                                            );
                                                            println!("Updated last SOL price in USDC => {:?}", last_sol_price_in_usdc);
                                                        }
                                                    } else if (buy_is_sol || sell_is_sol) {
                                                        // No side is USDC, but one side is SOL => use bridging if we have a known SOL price
                                                        if let Some(sol_price) = last_sol_price_in_usdc {
                                                            // Compute 1 token in SOL
                                                            let token_in_sol = if buy_is_sol {
                                                                // buy_amount is SOL, sell_amount is token => 1 token = (SOL / token)
                                                                buy_amount / sell_amount
                                                            } else {
                                                                // sell_is_sol => sell_amount is SOL, buy_amount is token => 1 token = (SOL / token)
                                                                sell_amount / buy_amount
                                                            };

                                                            let token_name = if buy_is_sol {
                                                                sell_symbol
                                                            } else {
                                                                buy_symbol
                                                            };

                                                            let token_price_in_usdc = token_in_sol * sol_price;
                                                            println!(
                                                                "Token {} price in USDC (via SOL bridging): {}",
                                                                token_name, token_price_in_usdc
                                                            );
                                                        }
                                                    } else {
                                                        // Neither side is USDC or SOL, skip or handle bridging logic for other tokens if desired
                                                        continue;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            },
                            Err(e) => {
                                eprintln!("Failed to decode DexParsedBlockMessage: {}", e);
                            }
                        }
                    }

                    consumer.commit_message(&msg, CommitMode::Async)?;
                }
                Err(e) => {
                    eprintln!("Error receiving message from Kafka: {}", e);
                }
            },
            Err(_) => {
                // Timeout triggered
                println!("No new messages within 10 seconds...");
            }
        }
    }
}

fn calculate_trade_price(event: &DexTradeEvent) -> Option<f64> {
    let (buy_side, sell_side) = (event.buy.as_ref()?, event.sell.as_ref()?);

    let buy_amount = buy_side.amount as f64
        / 10f64.powi(buy_side.currency.as_ref()?.decimals as i32);
    let sell_amount = sell_side.amount as f64
        / 10f64.powi(sell_side.currency.as_ref()?.decimals as i32);

    if buy_amount == 0.0 {
        return None;
    }

    // Example: price = quote / base
    let price = sell_amount / buy_amount;
    Some(price)
}
