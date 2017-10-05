/// Server should handle requests similar to Redis
/// 
/// List of commands:
/// -------------------------------------------

static HELP_STR : &str = "PING, INFO, USE [db], CREATE [db],
ADD [ts],[seq],[is_trade],[is_bid],[price],[size];
BULKADD ...; DDAKLUB
FLUSH, FLUSHALL, GETALL, GET [count], CLEAR
";

use conf;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::net::TcpStream;
use std::path::Path;
use std::thread;
use std::str;
use std::fs;

use dtf;

/// name: *should* be the filename
/// in_memory: are the updates read into memory?
/// size: true number of items
/// v: vector of updates
/// 
/// 
/// When client connects, the following happens:
/// 
/// 1. server creates a State
/// 2. initialize 'default' data store
/// 3. reads filenames under dtf_folder
/// 4. loads metadata but not updates
/// 5. client can retrieve server status using INFO command
/// 
/// When client adds some updates using ADD or BULKADD,
/// size increments and updates are added to memory
/// finally, call FLUSH to commit to disk the current store or FLUSHALL to commit all available stores.
/// the client can free the updates from memory using CLEAR or CLEARALL
/// 
struct Store {
    name: String,
    folder: String,
    in_memory: bool,
    size: u64,
    v: Vec<dtf::Update>,
}

impl Store {
    /// Push a new `Update` into the vec
    fn add(&mut self, new_vec : dtf::Update) {
        self.v.push(new_vec);
        self.size += 1;
    }

    /// Map vec of updates into JSON lists of objects
    /// 
    /// example:
    /// [{"ts":1505177459.658,"seq":139010,"is_trade":true,"is_bid":true,"price":0.0703629,"size":7.6506424}]
    fn to_string(&self, count:i32) -> String {
        let objects : Vec<String> = match count {
            -1 => self.v.clone().into_iter().map(|up| up.to_json()).collect(),
            n => self.v.clone().into_iter().take(n as usize).map(|up| up.to_json()).collect()
        };

        format!("[{}]\n", objects.join(","))
    }

    /// write items stored in memory into file
    /// If file exists, use append which only appends a filtered set of updates whose timestamp is larger than the old timestamp
    /// If file doesn't exists, simply encode.
    /// 
    /// TODO: Need to figure out how to specify symbol (and exchange name).
    fn flush(&self) -> Option<bool> {
        let fname = format!("{}/{}.dtf", self.folder, self.name);
        if Path::new(&fname).exists() {
            dtf::append(&fname, &self.v);
            return Some(true);
        }
        dtf::encode(&fname, &self.name /*XXX*/, &self.v);
        Some(true)
    }

    /// load items from dtf file
    fn load(&mut self) {
        let fname = format!("{}/{}.dtf", self.folder, self.name);
        if Path::new(&fname).exists() {
            self.v = dtf::decode(&fname);
            self.size = self.v.len() as u64;
            self.in_memory = true;
        }
    }

    /// load size from file
    fn load_size_from_file(&mut self) {
        let header_size = dtf::get_size(&format!("{}/{}", self.folder, self.name));
        self.size = header_size;
    }

    /// clear the vector. toggle in_memory. update size
    fn clear(&mut self) {
        self.v.clear();
        self.in_memory = false;
        self.load_size_from_file();
    }
}


/// Each client gets its own State
struct State {
    is_adding: bool,
    store: HashMap<String, Store>,
    current_store_name: String,
    dtf_folder: String
}

/// Parses a line that looks like 
/// 
/// 1505177459.658, 139010, t, t, 0.0703629, 7.65064249;
/// 
/// into an `Update` struct.
/// 
fn parse_line(string : &str) -> Option<dtf::Update> {
    let mut u = dtf::Update { ts : 0, seq : 0, is_bid : false, is_trade : false, price : -0.1, size : -0.1 };
    let mut buf : String = String::new();
    let mut count = 0;
    let mut most_current_bool = false;

    for ch in string.chars() {
        if ch == '.' && count == 0 {
            continue;
        } else if ch == '.' && count != 0 {
            buf.push(ch);
        } else if ch.is_digit(10) {
            buf.push(ch);
        } else if ch == 't' || ch == 'f' {
            most_current_bool = ch == 't';
        } else if ch == ',' || ch == ';' {
            match count {
                0 => { u.ts       = match buf.parse::<u64>() {Ok(ts) => ts, Err(_) => return None}},
                1 => { u.seq      = match buf.parse::<u32>() {Ok(seq) => seq, Err(_) => return None}},
                2 => { u.is_trade = most_current_bool; },
                3 => { u.is_bid   = most_current_bool; },
                4 => { u.price    = match buf.parse::<f32>() {Ok(price) => price, Err(_) => return None} },
                5 => { u.size     = match buf.parse::<f32>() {Ok(size) => size, Err(_) => return None}},
                _ => panic!("IMPOSSIBLE")
            }
            count += 1;
            buf.clear();
        }
    }
    Some(u)
}

fn gen_response(string : &str, state: &mut State) -> Option<String> {
    match string {
        "" => Some("".to_owned()),
        "PING" => Some("PONG.\n".to_owned()),
        "HELP" => Some(HELP_STR.to_owned()),
        "INFO" => {
            let info_vec : Vec<String> = state.store.values().map(|store| {
                format!(r#"{{"name": "{}", "in_memory": {}, "count": {}}}"#, store.name, store.in_memory, store.size)
            }).collect();

            Some(format!("[{}]\n", info_vec.join(", ")))
        },
        "BULKADD" => {
            state.is_adding = true;
            Some("".to_owned())
        },
        "DDAKLUB" => {
            state.is_adding = false;
            Some("1\n".to_owned())
        },
        "GETALL" => {
            Some(state.store.get_mut(&state.current_store_name).unwrap().to_string(-1))
        },
        "CLEAR" => {
            let current_store = state.store.get_mut(&state.current_store_name).expect("KEY IS NOT IN HASHMAP");
            current_store.clear();
            Some("1\n".to_owned())
        },
        "CLEARALL" => {
            for store in state.store.values_mut() {
                store.clear();
            }
            Some("1\n".to_owned())
        },
        "FLUSH" => {
            let current_store = state.store.get_mut(&state.current_store_name).expect("KEY IS NOT IN HASHMAP");
            current_store.flush();
            Some("1\n".to_owned())
        },
        "FLUSHALL" => {
            for store in state.store.values() {
                store.flush();
            }
            Some("1\n".to_owned())
        },
        _ => {
            // bulkadd and add
            if state.is_adding {
                let parsed = parse_line(string);
                match parsed {
                    Some(up) => {
                        let current_store = state.store.get_mut(&state.current_store_name).expect("KEY IS NOT IN HASHMAP");
                        current_store.add(up);
                    }
                    None => return None
                }
                Some("".to_owned())
            } else

            if string.starts_with("ADD ") {
                let data_string : &str = &string[3..];
                match parse_line(&data_string) {
                    Some(up) => {
                        let current_store = state.store.get_mut(&state.current_store_name).expect("KEY IS NOT IN HASHMAP");
                        current_store.v.push(up);
                    }
                    None => return None
                }
                Some("1\n".to_owned())
            } else 

            // db commands
            if string.starts_with("CREATE ") {
                let dbname : &str = &string[7..];
                state.store.insert(dbname.to_owned(), Store {
                    name: dbname.to_owned(),
                    v: Vec::new(),
                    size: 0,
                    in_memory: false,
                    folder: state.dtf_folder.clone()
                });
                Some(format!("Created DB `{}`.\n", &dbname))
            } else

            if string.starts_with("USE ") {
                let dbname : &str = &string[4..];
                if state.store.contains_key(dbname) {
                    state.current_store_name = dbname.to_owned();
                    let current_store = state.store.get_mut(&state.current_store_name).unwrap();
                    current_store.load();
                    Some(format!("SWITCHED TO DB `{}`.\n", &dbname))
                } else {
                    Some(format!("ERR unknown DB `{}`.\n", &dbname))
                }
            } else

            // get
            if string.starts_with("GET ") {
                let num : &str = &string[4..];
                let count = num.parse::<i32>().unwrap();
                let current_store = state.store.get_mut(&state.current_store_name).unwrap();
                Some(current_store.to_string(count))
            }

            else {
                Some(format!("ERR unknown command '{}'.\n", &string))
            }
        }
    }
}

/// Read config file and get folder name
/// dtf_folder is a folder in which the dtf files live
fn get_dtf_folder() -> String {
    let configs = conf::get_config();
    let dtf_folder = configs.get("dtf_folder").unwrap();
    dtf_folder.to_owned()
}

fn create_dir_if_not_exist(dtf_folder : &str) {
    if !Path::new(dtf_folder).exists() {
        fs::create_dir(dtf_folder).unwrap();
    }
}

/// Iterate through the dtf files in the folder and load some metadata into memory.
/// Create corresponding Store objects in State.
fn init_dbs(dtf_folder : &str, state: &mut State) {
    for dtf_file in fs::read_dir(&dtf_folder).unwrap() {
        let dtf_file = dtf_file.unwrap();
        let fname_os = dtf_file.file_name();
        let fname = fname_os.to_str().unwrap();
        if fname.ends_with(".dtf") {
            let name = Path::new(&fname_os).file_stem().unwrap().to_str().unwrap();
            let header_size = dtf::get_size(&format!("{}/{}", dtf_folder, fname));
            state.store.insert(name.to_owned(), Store {
                folder: dtf_folder.to_owned(),
                name: name.to_owned(),
                v: Vec::new(),
                size: header_size,
                in_memory: false
            });
        }
    }
}

fn handle_client(mut stream: TcpStream) {
    let dtf_folder = get_dtf_folder();
    create_dir_if_not_exist(&dtf_folder);


    let mut state = State {
        current_store_name: "default".to_owned(),
        is_adding: false,
        store: HashMap::new(),
        dtf_folder: dtf_folder.to_owned()
    };
    state.store.insert("default".to_owned(), Store {
        name: "default".to_owned(),
        v: Vec::new(),
        size: 0,
        in_memory: false,
        folder: dtf_folder.to_owned()
    });

    init_dbs(&dtf_folder, &mut state);

    let mut buf = [0; 2048];
    loop {
        let bytes_read = stream.read(&mut buf).unwrap();
        if bytes_read == 0 { break }
        let req = str::from_utf8(&buf[..(bytes_read-1)]).unwrap();

        let resp = gen_response(&req, &mut state);
        match resp {
            Some(str_resp) => {
                stream.write(str_resp.as_bytes()).unwrap()
                // stream.write(b">>> ").unwrap()
            }
            None => stream.write("ERR.".as_bytes()).unwrap()
        };
    }
}

pub fn run_server() {
    let addr = "127.0.0.1:9001";
    let listener = TcpListener::bind(addr).unwrap();
    println!("Listening on addr: {}", addr);

    for stream in listener.incoming() {
        let stream = stream.unwrap();
        thread::spawn(move || {
//             stream.write(b"
// Tectonic Shell v0.0.1
// Enter `HELP` for more options.
// >>> ").unwrap();
            handle_client(stream);
        });
    }
}

#[test]
fn should_parse_string_not_okay() {
    let string = "1505177459.658, 139010,,, f, t, 0.0703629, 7.65064249;";
    assert!(parse_line(&string).is_none());
}

#[test]
fn should_parse_string_okay() {
    let string = "1505177459.658, 139010, f, t, 0.0703629, 7.65064249;";
    let target = dtf::Update {
        ts: 1505177459658,
        seq: 139010,
        is_trade: false,
        is_bid: true,
        price: 0.0703629,
        size: 7.65064249
    };
    assert_eq!(target, parse_line(&string).unwrap());

    let string1 = "1505177459.650, 139010, t, f, 0.0703620, 7.65064240;";
    let target1 = dtf::Update {
        ts: 1505177459650,
        seq: 139010,
        is_trade: true,
        is_bid: false,
        price: 0.0703620,
        size: 7.65064240
    };
    assert_eq!(target1, parse_line(&string1).unwrap());
}