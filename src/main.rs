mod app;
mod blocklist;
mod config;
mod hosts;
mod patch;
mod win;

fn main() {
    if !win::is_elevated() {
        if win::relaunch_elevated() {
            return;
        }
        println!("lunar-adblock requires administrator rights. run the program as administrator.");
        std::thread::sleep(std::time::Duration::from_secs(4));
        return;
    }

    let cfg = config::load();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|dir| dir.to_path_buf()));
    let custom_path = exe_dir.map(|dir| dir.join("blocklist.txt"));
    let custom = blocklist::load_custom(custom_path.as_deref());
    let hosts = hosts::HostsFile::new(hosts::HOSTS_PATH);

    let mut app = app::App::new(cfg, hosts, custom);
    app.run();
}
