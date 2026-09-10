fn main() {
    let args: Vec<String> = std::env::args().collect();
    let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("help");
    match subcmd {
        "status"  => println!("Status: not yet implemented"),
        "pair"    => println!("Pair: not yet implemented"),
        "devices" => println!("Devices: not yet implemented"),
        "layout"  => println!("Layout: not yet implemented"),
        "config"  => println!("Config: not yet implemented"),
        _         => println!("Usage: manguesechee-cli <status|pair|devices|layout|config>"),
    }
}
