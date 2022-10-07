use tokio::net::TcpStream;
use tokio::net::TcpListener;
use std::error::Error;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::net::ToSocketAddrs;
use std::path::PrefixComponent;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use dialoguer;
use std::net::IpAddr;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use std::iter;

//extern crate pancurses;
use pancurses;

async fn async_io_thread(tx: mpsc::Sender<Command>, mut rx: mpsc::Receiver<ConsoleCommand>) -> Result<(), Box<dyn Error>> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    
    let stdin_reader = tokio::io::BufReader::new(stdin);

    let mut stdin_lines = stdin_reader.lines();

    // Wait for stdin while also printing messages from rx to stdout
    async fn read_line (stdin_lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::io::Stdin>>, stdout: &mut tokio::io::Stdout, rx: &mut mpsc::Receiver<ConsoleCommand>) -> Result<Option<String>, std::io::Error> {
        let line;
        loop {
            tokio::select! {
                l = stdin_lines.next_line() => {
                    line = Some(l);
                    break;
                },
                cmd = rx.recv() => {
                    if let Some(cmd) = cmd {
                        match cmd {
                            ConsoleCommand::Print {message, callback} => {
                                let _ = stdout.write_all(&message.into_bytes()).await;
                                let _ = stdout.flush().await;
                                let _ = callback.send(());
                            }
                        }
                    }
                }
            }
        }
        return line.unwrap();
    }

    async fn read_idx (values: &Vec<SocketAddr>, stdin_lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::io::Stdin>>, stdout: &mut tokio::io::Stdout, rx: &mut mpsc::Receiver<ConsoleCommand>) -> Result<usize, std::io::Error> {
        let values_len = values.len();

        let _idx = loop {
            let prompt = ["Select address by entering index:\n".to_string()]
                .into_iter()
                .chain(
                    values.iter()
                    .enumerate()
                    .map(|(i, val)| {
                        format!(" {}| {}\n", i, val)
                    })
                );

            stdout.write(&prompt.collect::<String>().into_bytes()).await?;
            stdout.flush().await?;

            let line_result = read_line(stdin_lines, stdout, rx).await?;
            
            if let Some(line_result) = line_result {
                let line_result = line_result.trim().to_string();
                
                match line_result.parse::<usize>() {
                    Err(err) => {
                        stdout.write_all(&format!("Not a valid index: {}\n", err).into_bytes()).await?;
                        stdout.flush().await?;
                    },
                    Ok(idx) => {
                        if (0..values_len).contains(&idx) {
                            break idx;
                        } else {
                            stdout.write_all(&format!("Not a valid index: index must be in range [0, {}>\n", values_len).into_bytes()).await?;
                            stdout.flush().await?;
                        }
                    }
                }
            }
        };

        Ok(1)
    }

    let target_addr = loop {
        stdout.write_all(&"Enter target <address>:<port> or <port> (127.0.0.1:<port> will be used with standalone port)\n".to_string().into_bytes()).await?;

        let user_input = read_line(&mut stdin_lines, &mut stdout, &mut rx).await;
        let user_input = match user_input {
            Err(err) => {
                stderr.write_all(&format!("Error reading input: {}\n", err).into_bytes()).await?;
                stderr.flush().await?;
                None
            },
            Ok(line) => {
                if line.is_none() {
                    None
                } else {
                    let line = line.unwrap().trim().to_string();
                    match line.to_lowercase().as_str() {
                        "\\exit" => {
                            break None;
                        },
                        _ => Some(line),
                    }
                }
            }
        };

        if user_input.is_none() {
            continue;
        }
        let user_input = user_input.unwrap();
        let addr_result = user_input.to_socket_addrs();
        let selected_addr = match addr_result {
            Err(err) => {
                let port = user_input.parse::<u16>();

                if let Ok(port_int) = port {
                    Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port_int))
                } else {
                    stdout.write_all(&format!("Error parsing address: {}", err).into_bytes()).await?;
                    stdout.flush().await?;
                    None
                }
            },
            Ok(addr_iter) => {
                let addr_vec: Vec<SocketAddr> = addr_iter.collect();

                if addr_vec.len() == 1 {
                    Some(addr_vec[0])
                } else {
                    let addr = match read_idx(&addr_vec, &mut stdin_lines, &mut stdout, &mut rx).await {
                        Err(err) => {
                            stdout.write_all(&format!("Error parsing index: {}", err).into_bytes()).await?;
                            stdout.flush().await?;
                            None
                        },
                        Ok(idx) => Some(addr_vec[idx]),
                    };
                    addr
                }
            }
        };

        if let Some(target_addr) = selected_addr {
            break Some(target_addr);
        }
    };

    if target_addr.is_none() {
        tx.send(Command::Exit).await?;
        return Ok(())
    }

    let target_addr = target_addr.unwrap();

    tx.send(Command::SelectTarget { addr: target_addr }).await?;


    loop {
        stdout.write_all(&"Enter <address>:<port> or <port> (0.0.0.0 will be used) to listen to or \\exit\n".to_string().into_bytes()).await?;
        stdout.flush().await?;
        let user_input_result = read_line(&mut stdin_lines, &mut stdout, &mut rx).await;

        if let Err(err) = user_input_result {
            stderr.write_all(&format!("Error reading input: {}", err).into_bytes()).await?;
            stderr.flush().await?;
            continue;
        }

        let user_input = user_input_result.unwrap();

        if user_input.is_none() {
            continue;
        }

        let user_input = user_input.unwrap();

        if user_input.to_lowercase().eq("\\exit") {
            tx.send(Command::Exit).await?;
            break;
        }

        let listened_addr_result = user_input.to_socket_addrs();
        let listened_addrs = match listened_addr_result {
            Err(err) => {
                let port = user_input.parse::<u16>();

                if let Ok(port_int) = port {
                    Some(vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), port_int)])
                } else {
                    stdout.write_all(&format!("Error parsing address: {}", err).into_bytes()).await?;
                    stdout.write_all(&format!("Try again:").into_bytes()).await?;
                    stdout.flush().await?;
                    None
                }
            },
            Ok(addr_iter) => {
                let addr_vec: Vec<SocketAddr> = addr_iter.collect();
                Some(addr_vec)
            }
        };

        if let Some(listened_addrs) = listened_addrs {

            let listened_addr = if listened_addrs.len() == 1 {
                Some(listened_addrs[0])
            } else {
                let listened_addr_idx = read_idx(&listened_addrs, &mut stdin_lines, &mut stdout, &mut rx).await;
                if let Err(err) = listened_addr_idx {
                    stderr.write(&format!("Error parsing input: {}\n", err).into_bytes()).await?;
                    stderr.flush().await?;
                    None
                } else {
                    let listened_addr_idx = listened_addr_idx.unwrap();
                    Some(listened_addrs[listened_addr_idx])
                }
            };

            if listened_addr.is_none() {
                continue;
            }
            let listened_addr = listened_addr.unwrap();

            let (sender, receiver) = oneshot::channel::<Result<SocketAddr, Box<dyn Error + Send>>>();
            tx.send(Command::ListenTo{addrs: listened_addr, resp: sender}).await?;
            let response = receiver.await;
            let response: Result<SocketAddr, Box<dyn Error + Send>> = match response {
                Err(err) => Err(Box::new(err)),
                Ok(r) => r,
            };
            match response {
                Err(err) => {
                    stdout.write(&format!("Error listening on address: {}\n", err).into_bytes()).await?;
                    stdout.flush().await?;
                },
                Ok(addr) => {
                    stdout.write(&format!("Listening on address: {}\n", addr).into_bytes()).await?;
                    stdout.flush().await?;
                }
            }
        }
    }

    Ok(())

}

async fn handle_connection(target_addr: SocketAddr, mut source_stream: TcpStream) {
    let target_stream_result = TcpStream::connect(target_addr).await;
    match target_stream_result {
        Err(err) => {
            eprintln!("Error connecting to target address {}", err);
        },
        Ok(mut target_stream) => {
            let (mut source_read, mut source_write) = source_stream.split();
            let (mut target_read, mut target_write) = target_stream.split();
            let _ = tokio::join!(
                tokio::io::copy(&mut source_read, &mut target_write),
                tokio::io::copy(&mut target_read, &mut source_write),
            );
        }
    }
}

async fn listen(target_addr: SocketAddr, source_addr: SocketAddr, responder: oneshot::Sender<Result<SocketAddr, Box<dyn Error + Send>>>) {
    let listener_result = TcpListener::bind(source_addr).await;
    match listener_result {
        Err(err) => {
            let _ = responder.send(Err(Box::new(err)));
        },
        Ok(listener) => {
            let _ = responder.send(match listener.local_addr() {
                Err(err) => Err(Box::new(err)),
                Ok(addr) => Ok(addr)
            });

            loop {
                match listener.accept().await {
                    Err(_) => {
                    },
                    Ok((source_stream, _source_address)) => {
                        tokio::task::spawn(handle_connection(target_addr, source_stream));
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
enum Command {
    Exit,
    SelectTarget {
        addr: SocketAddr,
    },
    ListenTo {
        addrs: SocketAddr,
        resp: oneshot::Sender<Result<SocketAddr, Box<dyn Error + Send>>>,
    }
}

#[derive(Debug)]
enum ConsoleCommand {
    Print { message: String, callback: oneshot::Sender<()> }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let (tx, mut rx) = mpsc::channel::<Command>(32);
    let (console_tx, mut console_rx) = mpsc::channel::<ConsoleCommand>(32);

    let prompt_tx = tx.clone();
    // let prompt_thread = tokio::task::spawn_blocking(move || {
    //     let err = io_thread(prompt_tx).err();

    //     if let Some(err) = err {
    //         println!("Program encountered error: {}", err);
    //     }
    // });
    let prompt_thread = tokio::task::spawn( async move {
        let err = async_io_thread(prompt_tx, console_rx).await.err();

        if let Some(err) = err {
            println!("Program encountered error: {}", err);
        }
    });

    let select_target = rx.recv().await.unwrap();

    let target_addr = match select_target {
        Command::SelectTarget { addr } => addr,
        Command::Exit => {
            println!("Shutting down");
            return Ok(())
        }
        _ => panic!("Expected the first command to be SelectTarget")
    };
    loop {
        let command = rx.recv().await.unwrap();

        match command {
            Command::Exit => break,
            Command::ListenTo { addrs, resp } => {
                tokio::task::spawn(async move {
                    listen(target_addr, addrs, resp).await;
                });
            },
            _ => panic!("Didn't expect SelectTarget command"),
        }
    };

    prompt_thread.await?;

    //prompt_thread.abort();
    println!("Shutting down");

    Ok(())
}
