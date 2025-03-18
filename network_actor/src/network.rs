use serde_json;
use std::collections::HashMap;
use std::{
    fmt,
    io::{stdout, Write},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    sync::{broadcast, mpsc, Mutex},
    time::sleep,
};

use crate::{KVCommand, KeyValue, Message, CLIENT_PORTS, PORT_MAPPINGS};

pub async fn run() {
    // setup client sockets to talk to nodes
    let api_sockets = Arc::new(Mutex::new(HashMap::new()));
    for port in CLIENT_PORTS.iter() {
        let api_sockets = api_sockets.clone();
        tokio::spawn(async move {
            loop {
                let api_sockets = api_sockets.clone();
                let join_handler = tokio::spawn(async move {
                    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))
                        .await
                        .unwrap();
                    let (socket, _addr) = listener.accept().await.unwrap();
                    let (reader, writer) = socket.into_split();
                    api_sockets.lock().await.insert(port, writer);
                    // receiver actor
                    let join_reader = tokio::spawn(async move {
                        let mut reader = BufReader::new(reader);
                        loop {
                            let mut data = vec![];
                            let bytes_read = reader.read_until(b'\n', &mut data).await.unwrap();
                            if bytes_read == 0 {
                                // dropped socket EOF
                                println!("{port} disconnected");
                                api_sockets.lock().await.remove(port);
                                break;
                            }
                            if let Ok(msg) = serde_json::from_slice::<Message>(&data) {
                                println!("From {}: {:?}", port, msg); // TODO: handle APIResponse
                            }
                        }
                    });
                    join_reader.await.unwrap();
                });
                join_handler.await.unwrap();
            }
        });
    }

    // Handle user input to propose values
    let api = api_sockets.clone();
    tokio::spawn(async move {
        loop {
            // Get input
            let mut input = String::new();
            print!("Type a command here <put/delete/get> <args>: ");
            let _ = stdout().flush();
            let mut reader = BufReader::new(tokio::io::stdin());
            reader
                .read_line(&mut input)
                .await
                .expect("Did not enter a string");

            // Parse and send command
            match parse_command(input) {
                Ok((command, None)) => {
                    let mut sent_command = false;
                    for port in CLIENT_PORTS.iter() {
                        if let Some(writer) = api.lock().await.get_mut(port) {
                            let cmd = Message::APIRequest(command.clone());
                            let mut data =
                                serde_json::to_vec(&cmd).expect("could not serialize cmd");
                            data.push(b'\n');
                            writer.write_all(&data).await.unwrap();
                            sent_command = true;
                            break;
                        }
                    }
                    if !sent_command {
                        println!("Couldn't send command, no node is reachable");
                    }
                }
                Ok((command, Some(port))) => {
                    if let Some(writer) = api.lock().await.get_mut(&port) {
                        let cmd = Message::APIRequest(command.clone());
                        let mut data = serde_json::to_vec(&cmd).expect("could not serialize cmd");
                        data.push(b'\n');
                        writer.write_all(&data).await.unwrap();
                    }
                }
                Err(err) => println!("{err}"),
            }
            // Wait some amount of time for cluster response
            sleep(Duration::from_millis(50)).await;
        }
    });

    // setup intra-cluster communication
    let partitions: Arc<Mutex<Vec<(u64, u64, f32)>>> = Arc::new(Mutex::new(vec![]));
    let mut out_channels = HashMap::new();
    for port in PORT_MAPPINGS.keys() {
        // Create an initial broadcast channel per port
        let (sender, _rec) = broadcast::channel::<Vec<u8>>(10000);
        let sender = Arc::new(sender);
        out_channels.insert(*port, sender.clone());
    }
    let out_channels = Arc::new(Mutex::new(out_channels));

    let (central_sender, mut central_receiver) = mpsc::channel(10000);
    let central_sender = Arc::new(central_sender);

    // For each port in PORT_MAPPINGS, spawn a task to handle connections.
    for port in PORT_MAPPINGS.keys() {
        let out_chans = out_channels.clone();
        let central_sender = central_sender.clone();
        tokio::spawn(async move {
            loop {
                // Bind a listener on the given port.
                let listener = TcpListener::bind(format!("0.0.0.0:{}", port))
                    .await
                    .unwrap();
                // Accept an incoming connection.
                let (socket, _addr) = listener.accept().await.unwrap();
                let (reader, writer) = socket.into_split();
                println!("Connected to port {}", port);

                // Create a new broadcast channel for this connection and insert it.
                let (new_sender, _rec) = broadcast::channel::<Vec<u8>>(10000);
                let new_sender = Arc::new(new_sender);
                {
                    let mut out_map = out_chans.lock().await;
                    out_map.insert(*port, new_sender.clone());
                }

                // Spawn the sender actor: it subscribes to the broadcast and writes data to the TCP writer.
                let out_chans_sender = out_chans.clone();
                let sender_task = tokio::spawn({
                    let new_sender = new_sender.clone();
                    async move {
                        let mut rx = new_sender.subscribe();
                        // We take ownership of writer here.
                        let mut writer = writer;
                        while let Ok(data) = rx.recv().await {
                            if let Err(e) = writer.write_all(&data).await {
                                println!("Error writing to port {}: {:?}", port, e);
                                break;
                            }
                        }
                        println!("Sender actor finished for port {}", port);
                    }
                });

                // Spawn the receiver actor: it reads from the TCP stream and sends messages to the central channel.
                let central_sender_clone = central_sender.clone();
                let receiver_task = tokio::spawn(async move {
                    let mut reader = BufReader::new(reader);
                    loop {
                        let mut data = vec![];
                        match reader.read_until(b'\n', &mut data).await {
                            Ok(0) => {
                                // EOF detected: the connection has dropped.
                                println!("EOF: Connection dropped on port {} (receiver actor)", port);
                                break;
                            }
                            Ok(_) => {
                                if let Some(mapped_port) = PORT_MAPPINGS.get(&port) {
                                    if let Err(e) = central_sender_clone.send((port, *mapped_port, data)).await {
                                        println!("Error sending from central sender on port {}: {:?}", port, e);
                                        break;
                                    }
                                }
                            }
                            Err(e) => {
                                println!("Error reading from port {}: {:?}", port, e);
                                break;
                            }
                        }
                    }
                    println!("Receiver actor finished for port {}", port);
                });

                // Wait until either the sender or receiver task completes.
                tokio::select! {
                    _ = sender_task => {
                        println!("Sender task finished for port {}. Cancelling receiver task...", port);
                    },
                    _ = receiver_task => {
                        println!("Receiver task finished for port {}. Cancelling sender task...", port);
                    }
                }

                // Clean up: remove the stale broadcast channel for this port.
                {
                    let mut out_map = out_chans.lock().await;
                    out_map.remove(&port);
                }
                println!("Resetting connection handling for port {}", port);
                // Loop will now restart and bind a new listener for this port.
            }
        });
    }

    // // setup intra-cluster communication
    // let partitions: Arc<Mutex<Vec<(u64, u64, f32)>>> = Arc::new(Mutex::new(vec![]));
    // let mut out_channels = HashMap::new();
    // for port in PORT_MAPPINGS.keys() {
    //     let (sender, _rec) = broadcast::channel::<Vec<u8>>(10000);
    //     let sender = Arc::new(sender);
    //     out_channels.insert(*port, sender.clone());
    // }
    // let out_channels = Arc::new(out_channels);

    // let (central_sender, mut central_receiver) = mpsc::channel(10000);
    // let central_sender = Arc::new(central_sender);

    // for port in PORT_MAPPINGS.keys() {
    //     let out_chans = out_channels.clone();
    //     let central_sender = central_sender.clone();
    //     tokio::spawn(async move {
    //         while true {
    //             let join_handler = tokio::spawn({
    //                 let central_sender = central_sender.clone(); // Clone inside the loop
    //                 let out_chans = out_chans.clone(); // Clone inside the loop
    //                 async move {
    //                     let listener = TcpListener::bind(format!("0.0.0.0:{}", port))
    //                         .await
    //                         .unwrap();
    //                     let (socket, _addr) = listener.accept().await.unwrap();
    //                     let (reader, mut writer) = socket.into_split();
    //                     println!("Connected to port {}", port);

    //                     // Sender actor
    //                     let out_channels = out_chans.clone();
    //                     tokio::spawn(async move {
    //                         if let Some(sender) = out_channels.get(&port) {
    //                             let mut receiver = sender.clone().subscribe();
    //                             while let Ok(data) = receiver.recv().await {
    //                                 let _ = writer.write_all(&data).await;
    //                             }
    //                             println!("Disconnected from port {} (sender actor)", port);
    //                             out_channels.remove(&port);
    //                         }
    //                     });

    //                     // Receiver actor
    //                     let central_sender = central_sender.clone();
    //                     let join_reader = tokio::spawn(async move {
    //                         let mut reader = BufReader::new(reader);
    //                         loop {
    //                             let mut data = vec![];
    //                             if let Ok(_) = reader.read_until(b'\n', &mut data).await {
    //                                 if let Some(mapped_port) = PORT_MAPPINGS.get(&port) {
    //                                     if let Err(_e) =
    //                                         central_sender.send((port, *mapped_port, data)).await
    //                                     {
    //                                         break;
    //                                     }
    //                                 }
    //                             } else {
    //                                 println!("Disconnected from port {} (receiver actor)", port);
    //                                 break;
    //                             }
    //                         }
    //                     });
    //                     join_reader.await.unwrap();
    //                 }
    //             });

    //             join_handler.await.unwrap();
    //         }
    //     });
    // }

    // the one central actor that sees all messages
    while let Some((from_port, to_port, msg)) = central_receiver.recv().await {
        // drop message if network is partitioned between sender and receiver
        for (from, to, _probability) in partitions.lock().await.iter() {
            if from == from_port && to == &to_port {
                continue;
            }
        }

        loop {
            let sender = {
                let map = out_channels.lock().await;
                map.get(&to_port).cloned() // Cloning outside of lock
            };

            //TODO: May need to add some asyncrhony here to allow messages to be sent even if a node is down. Otherwise a down node will lock the whole system.
            if let Some(sender) = sender {
                let _ = sender.send(msg);
                break;
            }
            
            sleep(Duration::from_millis(100)).await;
        }

        //let sender = out_channels.lock().await.get(&to_port).unwrap().clone();
        // let _ = sender.send(msg);
    }
}

struct ParseCommandError(String);
impl fmt::Display for ParseCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
fn parse_command(line: String) -> Result<(KVCommand, Option<u64>), ParseCommandError> {
    let mut words = line.trim().split(" ");
    let command_type = words
        .next()
        .ok_or(ParseCommandError("Not enough arguments".to_string()))?;

    let command = match command_type {
        "delete" => {
            let value = words
                .next()
                .ok_or(ParseCommandError("Not enough arguments".to_string()))?;
            let port = words.next().map(|x| x.parse::<u64>().unwrap());
            (KVCommand::Delete(value.to_string()), port.into())
        }
        "get" => {
            let value = words
                .next()
                .ok_or(ParseCommandError("Not enough arguments".to_string()))?;
            let port = words.next().map(|x| x.parse::<u64>().unwrap());
            (KVCommand::Get(value.to_string()), port.into())
        }
        "put" => {
            let key = words
                .next()
                .ok_or(ParseCommandError("Not enough arguments".to_string()))?
                .to_string();
            let value = words
                .next()
                .ok_or(ParseCommandError("Not enough arguments".to_string()))?
                .to_string();
            let port = words.next().map(|x| x.parse::<u64>().unwrap());
            (KVCommand::Put(KeyValue { key, value }), port.into())
        }
        "help" => {
            return Err(ParseCommandError(
                "Commands: put <key> <value>, get <key>, delete <key> (optional <port>)".into(),
            ));
        }
        _ => Err(ParseCommandError("Invalid command type".to_string()))?,
    };
    Ok(command)
}
