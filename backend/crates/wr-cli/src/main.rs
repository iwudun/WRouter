//! wr —— WRouter CLI 入口(方案 #14 R1.12 / R1.5)。
//!
//! 子命令面与 Python `cli.py` 对齐:
//! - `wr serve [--host --port --data-dir --ready-file --parent-pid
//!   --serve-ui --ui-dir]`(裸 `wr` 等价于 serve);
//! - `wr --version` / `wr version`(R1.5,版本来自构建时 WR_VERSION,
//!   源头是根 package.json);
//! - `wr doctor`(占位:输出有效装配信息,诊断块随 R6 交付前补齐)。
//!
//! 与 Python 的差异(有意):Python cli 的 APPImage 诊断块是 Linux
//! 打包临时排障产物,Rust 侧不搬,由 doctor 承载演进后的诊断面。

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "wr", version = wr_server::VERSION, about = "WRouter local AI gateway")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 启动 WRouter 后端(管理面 + 网关面)
    Serve {
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long, value_name = "DIR")]
        data_dir: Option<PathBuf>,
        #[arg(long, value_name = "FILE")]
        ready_file: Option<PathBuf>,
        #[arg(long)]
        parent_pid: Option<u32>,
        /// 开启管理面静态托管:浏览器 / Tauri 直接打开
        /// http://<host>:<port>/ 即为完整管理面(20261102_01 P1-6)。
        #[arg(long)]
        serve_ui: bool,
        /// 前端 dist 目录(覆盖 WROUTER_FRONTEND_DIR;
        /// 仅在 --serve-ui 开启时生效)。
        #[arg(long, value_name = "DIR")]
        ui_dir: Option<PathBuf>,
    },
    /// 打印版本号
    Version,
    /// 输出有效装配诊断信息
    Doctor,
}

fn main() {
    let cli = Cli::parse();
    let exit_code = match run(cli) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("[wr] {err:#}");
            1
        }
    };
    std::process::exit(exit_code);
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        None => cmd_serve(ServeArgs::default()),
        Some(Commands::Serve {
            host,
            port,
            data_dir,
            ready_file,
            parent_pid,
            serve_ui,
            ui_dir,
        }) => cmd_serve(ServeArgs {
            host,
            port,
            data_dir,
            ready_file,
            parent_pid,
            serve_ui,
            ui_dir,
        }),
        Some(Commands::Version) => {
            println!("wr {}", wr_server::VERSION);
            Ok(())
        }
        Some(Commands::Doctor) => cmd_doctor(),
    }
}

#[derive(Default)]
struct ServeArgs {
    host: Option<String>,
    port: Option<u16>,
    data_dir: Option<PathBuf>,
    ready_file: Option<PathBuf>,
    parent_pid: Option<u32>,
    serve_ui: bool,
    ui_dir: Option<PathBuf>,
}

/// 镜像 Python cli.py:CLI 参数覆盖同语义环境变量,再由
/// `Settings::from_env()` 统一装配。
fn cmd_serve(args: ServeArgs) -> anyhow::Result<()> {
    // Safety:进程启动早期(任何线程产生之前)写环境变量。
    unsafe {
        if let Some(host) = args.host {
            std::env::set_var("WROUTER_HOST", host);
        }
        if let Some(port) = args.port {
            std::env::set_var("WROUTER_WEB_PORT", port.to_string());
        }
        if let Some(data_dir) = args.data_dir {
            std::env::set_var("WROUTER_DATA_DIR", data_dir);
        }
        if let Some(ready_file) = args.ready_file {
            std::env::set_var("WROUTER_READY_FILE", ready_file);
        }
        if let Some(parent_pid) = args.parent_pid {
            std::env::set_var("WROUTER_PARENT_PID", parent_pid.to_string());
        }
        // 20261102_01 P1-6:静态托管开关与前端目录(仅在显式传参时写入,
        // 不传则保留 env 原值,保持「CLI 覆盖 env」的既有语义)。
        if args.serve_ui {
            std::env::set_var("WROUTER_SERVE_UI", "1");
        }
        if let Some(ui_dir) = args.ui_dir {
            std::env::set_var("WROUTER_FRONTEND_DIR", ui_dir);
        }
    }

    wr_server::logging::init();
    wr_server::install_panic_hook();
    let settings = wr_server::settings::Settings::from_env();
    tracing::info!(target: "wrouter.cli", "WRouter {} 启动(数据目录 {})", wr_server::VERSION, settings.data_dir.display());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(wr_server::run_serve(settings))
}

fn cmd_doctor() -> anyhow::Result<()> {
    wr_server::logging::init();
    let settings = wr_server::settings::Settings::from_env();
    println!("name           = {}", settings.app_name);
    println!("version        = {}", settings.version);
    println!("host           = {}", settings.host);
    println!("gateway_port   = {}", settings.gateway_port);
    println!("web_port       = {}", settings.web_port);
    println!("proxy_port     = {}", settings.proxy_port);
    println!("data_dir       = {}", settings.data_dir.display());
    println!("config_dir     = {}", settings.config_dir.display());
    println!(
        "ready_file     = {}",
        settings
            .ready_file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<unset>".to_owned())
    );
    println!(
        "parent_pid     = {}",
        settings
            .parent_pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "<unset>".to_owned())
    );
    // 令牌永不输出(C 系列约束):只报来源形态。
    println!(
        "auth_token     = {}",
        if std::env::var_os("WROUTER_WEB_AUTH_TOKEN").is_some() {
            "<from env>"
        } else {
            "<persisted or generated>"
        }
    );
    println!("body_retention_days = {}", settings.body_retention_days);
    println!("log_retention_days  = {}", settings.log_retention_days);
    // 20261102_01 P1-6:静态托管装配状态(是否托管前端 + 前端目录)。
    println!("serve_ui       = {}", settings.serve_ui);
    println!("frontend_dir   = {}", settings.frontend_dir.display());
    Ok(())
}
