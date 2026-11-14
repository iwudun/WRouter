//! WRouter 服务层(方案 #14 D14)。
//!
//! 依赖方向:`wr-server → {wr-app, wr-infra}`(装配时构造具体实现
//! 注入端口)。R1 提供最小服务骨架;R2 挂载数据面:
//! - [`management_router`]:管理面(Web 端口 3462),`GET /api/v1/health`;
//! - [`gateway_router`]:网关面(Gateway 端口 3460),`GET /health` +
//!   R2 协议路由(/v1/chat/completions 等,镜像 Python
//!   `app.py::create_gateway_app` 的路由表)与转发管线;
//! - [`gateway_dispatch`]:网关数据面 HTTP 分派(处理器 → 认证 →
//!   流式/非流式分流 → 转发管线,20261013_01 C2 自本文件独立);
//! - [`run_serve`]:双端口并发 + 优雅停机 + ready 文件(镜像 Python cli);
//! - [`logging`]:tracing 管道,行格式对齐 Python LOG_FORMAT;
//! - [`settings`]:环境变量装配,镜像 Python `settings.py`。
//!
//! CORS 注记(R2.1):Python 的 CORS 中间件只挂在**管理面** app
//! (`app.py:117-124`),gateway app 无任何 CORS/中间件 —— Rust 侧一致,
//! 管理面的 CORS 随管理面 API 阶段(R5)接入。
//! 请求体上限注记(20261016_01 A-4):Python/uvicorn 未配置入站体积
//! 限制,历史 Rust 侧 `DefaultBodyLimit::disable()` 对齐;现在默认
//! 64MiB(`WROUTER_MAX_BODY_BYTES` 可调,0 回默认),管理与网关两个
//! 面统一在读取阶段以 413 拒绝超大请求。

pub mod admin;
pub mod first_run;
pub mod gateway;
pub mod gateway_dispatch;
pub mod logging;
pub mod service_control;
pub mod settings;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::Router;
use serde_json::{Value, json};
use std::future::IntoFuture;

use gateway::{GatewayDeps, SharedGatewayDeps};
use gateway_dispatch::{
    AppState, gateway_anthropic_messages, gateway_gemini_interactions, gateway_gemini_model,
    gateway_health, gateway_models, gateway_openai_chat, gateway_openai_embeddings,
    gateway_openai_responses,
};

/// 运行时版本:构建时从根 package.json 注入(env WR_VERSION,见
/// packaging/build-rust.sh、scripts/build-desktop.mjs 与 scripts/dev.mjs);
/// 未注入时为开发占位。
pub const VERSION: &str = match option_env!("WR_VERSION") {
    Some(v) => v,
    None => "0.0.0-dev",
};

/// 管理面路由(Web 端口 3462)。R5 起全量管理 API(健康/能力/认证/REST/RPC/SSE)。
pub fn management_router(
    settings: Arc<settings::Settings>,
    deps: SharedGatewayDeps,
    store: std::sync::Arc<wr_infra::sqlite::SqliteStore>,
) -> Router {
    management_router_with_control(settings, deps, store, service_control::ServiceControl::new())
}

/// 管理面路由 + 服务控制通道(20261102_01 P6)。
///
/// 与 [`management_router`] 的唯一区别:注入**外部**的 [`service_control::ServiceControl`]
/// —— 让管理面的「暂停网关 / 停止服务」能真正作用到网关面任务与停机信号上。
/// 独立调用(测试)时用 `management_router` 即可(自带一条独立的控制通道)。
pub fn management_router_with_control(
    settings: Arc<settings::Settings>,
    deps: SharedGatewayDeps,
    store: std::sync::Arc<wr_infra::sqlite::SqliteStore>,
    control: service_control::ServiceControl,
) -> Router {
    // 启动时加载一次 capabilities(缺失/损坏降级到内联默认)。
    let capabilities = match std::fs::read_to_string(&settings.capabilities_file) {
        Ok(text) => serde_json::from_str::<Value>(&text).unwrap_or_else(|e| {
            tracing::warn!(target: "wrouter.admin", %e, "capabilities_parse_failed");
            capabilities_fallback(&settings)
        }),
        Err(e) => {
            tracing::warn!(target: "wrouter.admin", %e, "capabilities_load_failed");
            capabilities_fallback(&settings)
        }
    };
    // 入站体积上限(20261016_01 A-4):管理面与网关面同一策略。
    let body_limit = limit_max_body_bytes(&settings);
    let state = admin::AdminState {
        settings,
        deps,
        store,
        capabilities,
        control,
    };
    admin::admin_router(state).layer(DefaultBodyLimit::max(body_limit))
}

/// capabilities 文件缺失/损坏时的内联降级(镜像 app.py:39-49)。
///
/// 20261102_01 P1-8:`restEndpoints` 与
/// `contracts/wrouter-capabilities.json` 保持同步(含
/// `GET /api/wr/session`)—— 契约文件缺失时管理面仍能自报完整端点表。
fn capabilities_fallback(settings: &settings::Settings) -> Value {
    json!({
        "schema": "wrouter.capabilities",
        "version": 1,
        "protocols": ["openai.chat.completions","openai.responses","openai.embeddings","anthropic.messages"],
        "managementRpc": ["getConfig","saveConfig","getStatus","getProviders","probeProvider",
            "getProviderHealth","getRequestLogDetail","getCallLogs","getUsageStats",
            "getApiKeyLastUsed","getProfiles","applyProfile","stopProfile"],
        "restEndpoints": [
            "GET /api/v1/health",
            "GET /api/v1/capabilities",
            "GET /api/v1/status",
            "GET /api/v1/config",
            "PUT /api/v1/config",
            "GET /api/v1/config/export",
            "POST /api/v1/config/import",
            "GET /api/v1/providers",
            "PUT /api/v1/providers/{id}",
            "DELETE /api/v1/providers/{id}",
            "POST /api/v1/providers/probe",
            "GET /api/v1/providers/health",
            "POST /api/v1/providers/health/check",
            "GET /api/v1/metrics",
            "GET /api/v1/request-logs",
            "GET /api/v1/usage",
            "POST /api/wr/rpc",
            "GET /api/wr/session"
        ],
        "ports": {
            "gateway": settings.gateway_port,
            "management": settings.web_port,
            "proxy": settings.proxy_port,
        },
    })
}

/// 网关面路由(Gateway 端口 3460)。
///
/// 路由表镜像 Python `create_gateway_app`(app.py:359-414);gemini 的
/// `models/{model}:generateContent` 以整段参数 `{model_action}` 捕获后
/// 在处理器内拆分(matchit 不支持段内参数)。
pub fn gateway_router(settings: Arc<settings::Settings>, gateway: SharedGatewayDeps) -> Router {
    let body_limit = limit_max_body_bytes(&settings);
    let state = AppState {
        settings,
        gateway: Some(gateway),
    };
    Router::new()
        .route("/health", get(gateway_health))
        .route("/v1/chat/completions", post(gateway_openai_chat))
        .route("/v1/messages", post(gateway_anthropic_messages))
        .route("/v1/responses", post(gateway_openai_responses))
        .route("/v1/embeddings", post(gateway_openai_embeddings))
        // 2026-09-26 调用契约:选择器含 `/`(供应商/模型),路由参数改通配
        // 捕获整段(model:action),rsplit_once(':') 解析不变。
        .route("/v1beta/models/{*model_action}", post(gateway_gemini_model))
        .route("/v1/interactions", post(gateway_gemini_interactions))
        .route("/v1beta/interactions", post(gateway_gemini_interactions))
        .route("/v1/models", get(gateway_models))
        // 入站体积上限(20261016_01 A-4):替代此前的 disable(),默认
        // 64MiB,`WROUTER_MAX_BODY_BYTES` 可调。
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state)
}

/// 入站请求体上限(20261016_01 A-4):`WROUTER_MAX_BODY_BYTES` 解析值,
/// 0/缺省回落 64MiB 默认。
fn limit_max_body_bytes(settings: &settings::Settings) -> usize {
    if settings.max_body_bytes == 0 {
        settings::DEFAULT_MAX_BODY_BYTES
    } else {
        settings.max_body_bytes
    }
}

/// 写 ready 文件(镜像 Python cli.py:pid/host/port/gatewayPort/
/// apiVersion/authToken)。Electron 宿主据此连接后端。
fn write_ready_file(settings: &settings::Settings) -> anyhow::Result<()> {
    let Some(ready_file) = &settings.ready_file else {
        return Ok(());
    };
    if let Some(parent) = ready_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = json!({
        "pid": std::process::id(),
        "host": settings.host,
        "port": settings.web_port,
        "gatewayPort": settings.gateway_port,
        "apiVersion": "v1",
        "authToken": settings.auth_token,
    });
    std::fs::write(ready_file, serde_json::to_string(&payload)?)?;
    Ok(())
}

/// 管理面排空上限(秒):优雅停机等所有 in-flight 连接结束。
///
/// 管理面的 SSE 长连接已在停机时主动收流(见 `admin::status::events_endpoint`),
/// 这里只是**兜底** —— 防止其它异常长连接把进程永远钉住。
///
/// ⚠️ 这个上限**只在优雅停机开始之后**计时(见 `run_serve_with_shutdown`
/// 的 mgmt 段):它是「停机时不被长连接钉死」的兜底,不是「服务存活时长」。
/// 把它套在整个服务生命周期外面会让管理面启动后固定时长消失
/// (20261104 回归:页面全部 502)。
pub const MGMT_DRAIN_TIMEOUT_SECS: u64 = 5;

/// 管理面等待网关面收尾的上限(秒):超过就不再等,直接收管理面。
const MGMT_STOP_DEADLINE_SECS: u64 = 12;

/// 网关面排空上限(秒):网关转发可能是**长流式响应**(SSE / 流式对话),
/// 客户端不一定会主动断开 → 排空必须有上限,否则「暂停 / 停机」会一直等下去。
///
/// 取值需小于 systemd `TimeoutStopSec`(默认 90s),否则会被 SIGKILL 抢先。
const GATEWAY_DRAIN_TIMEOUT_SECS: u64 = 10;

/// 优雅停机信号:SIGINT(ctrl_c)与 SIGTERM(镜像 uvicorn 的停机语义)。
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!(target: "wrouter.cli", "收到停机信号,开始优雅停机");
}

/// 启动管理面 + 网关面双服务并阻塞到优雅停机完成(CLI 路径)。
///
/// R2:网关数据面依赖(sqlite 配置源 + 上游客户端 + 熔断/并发)在
/// 此装配(组合根,镜像 Python Container)。
///
/// 20261102_01 P3-2:退化为 [`run_serve_with_shutdown`] 的薄封装 ——
/// 停机信号仍是「SIGINT / SIGTERM」,**CLI 行为零变化**。
pub async fn run_serve(settings: settings::Settings) -> anyhow::Result<()> {
    run_serve_with_shutdown(settings, shutdown_signal()).await
}

/// 启动管理面 + 网关面双服务,停机信号由调用方注入。
///
/// 20261102_01 P3-2:Tauri 桌面壳把后端**同进程**跑起来(不再 spawn
/// `wr-backend` 子进程),窗口关闭时需要主动停机 —— 注入式停机信号
/// 让「桌面壳生命周期」与「服务生命周期」解耦:
///
/// - CLI(`wr serve`):传 [`shutdown_signal()`],语义与改造前完全一致;
/// - Tauri:传一个由窗口关闭事件驱动的 future。
///
/// 实现注记:原实现给管理面/网关面各挂了一个**独立的**
/// `shutdown_signal()`(两个 ctrl_c 监听)。抽成单一 future 后必须
/// 显式广播,否则只有一个 server 会收到停机信号 —— 这里用
/// `tokio::sync::watch` 做「一次触发、多方等待」。
pub async fn run_serve_with_shutdown<S>(
    settings: settings::Settings,
    shutdown: S,
) -> anyhow::Result<()>
where
    S: std::future::Future<Output = ()> + Send + 'static,
{
    write_ready_file(&settings)?;
    let shared = Arc::new(settings.clone());
    let config_source = Arc::new(wr_infra::sqlite_config::SqliteConfigSource::new(
        &settings.data_dir,
    ));
    // R4:读写门面(与只读配置源同库);写线程在内部启动。
    let store = Arc::new(
        wr_infra::sqlite::SqliteStore::open(&settings.data_dir)
            .await
            .map_err(|e| anyhow::anyhow!("open sqlite store failed: {e}"))?,
    );
    // 首启种入默认路由密钥(dev-open 移除后避免空注册表锁死;幂等/D2)。
    // 必须在 GatewayDeps::new 与首个快照之前,确保快照即含该密钥。
    first_run::ensure_default_routing_key(&store).await?;
    let gateway_deps: SharedGatewayDeps = Arc::new(GatewayDeps::new(
        shared.clone(),
        config_source,
        store.clone(),
    ));
    // 熔断状态从库里恢复(镜像 Python CircuitBreaker::reload)。
    gateway_deps.breaker.reload().await;
    gateway_deps.sync_runtime_config(&gateway_deps.snapshot().await.unwrap());
    // R4.11:日志保留/迁移后台任务(停机时 abort,已提交的清理自然落库)。
    let maintenance =
        gateway::maintenance::LogMaintenanceService::from_settings(store.clone(), &shared);
    let maintenance_task = tokio::spawn(maintenance.run_forever());
    let mgmt_listener =
        tokio::net::TcpListener::bind((settings.host.as_str(), settings.web_port)).await?;
    // 20261102_01 P6:网关面首次 bind 仍然「快速失败」(端口被占 → 启动失败,
    // 与改造前完全一致)。只有「暂停后再恢复」才允许 bind 失败 —— 那时记
    // error 状态、管理面继续服务(见 run_gateway_face)。
    let gw_listener =
        tokio::net::TcpListener::bind((settings.host.as_str(), settings.gateway_port)).await?;
    tracing::info!(target: "wrouter.cli", "WRouter {} 管理面 http://{}:{} 网关面 http://{}:{}", VERSION, settings.host, settings.web_port, settings.host, settings.gateway_port);
    // 单一停机信号 → 广播给管理面/网关面两个 server(见函数文档)。
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    // 「网关面已收尾」信号 → 管理面据此收尾(**顺序停机**,见 mgmt_stop)。
    let (mgmt_stop_tx, mgmt_stop_rx) = tokio::sync::watch::channel(false);
    // 「管理面优雅停机已开始」信号 → 排空上限从此刻起算(见 mgmt 段注释)。
    let (mgmt_drain_tx, mut mgmt_drain_rx) = tokio::sync::oneshot::channel::<()>();
    // 停机信号发送端的第二份副本:管理面意外结束时用它拉起整进程停机。
    let stop_tx_guard = stop_tx.clone();
    // 停机状态读取端:管理面结束时要判断「这是正常停机收尾还是意外结束」。
    let stop_state_rx = stop_rx.clone();
    // 20261102_01 P6:服务控制通道 —— 网关面启停 + 页面「停止服务」的内部停机请求。
    let control = service_control::ServiceControl::new();
    let stop_task = tokio::spawn({
        let mut internal_stop = control.stop_requested_rx();
        let control = control.clone();
        async move {
            // 两个来源任一触发即广播停机:
            // - 调用方注入的停机信号(桌面壳窗口关闭 / CLI 的 ctrl_c / systemd 的 SIGTERM);
            // - 页面「停止服务」的内部请求(P6)。
            tokio::select! {
                _ = shutdown => {
                    tracing::info!(target: "wrouter.cli", "收到停机信号,开始优雅停机");
                    // 同步置位停机请求 → 管理面 SSE 主动收流。
                    // 少了这一步,浏览器长连接会让优雅停机永远等不到连接结束
                    // (端口已关但进程残留,systemd 仍显示 running)。
                    control.request_stop();
                    let _ = stop_tx.send(true);
                }
                _ = wait_for_value(&mut internal_stop, true) => {
                    tracing::info!(target: "wrouter.cli", "收到页面停机请求,开始优雅停机");
                    let _ = stop_tx.send(true);
                }
            }
        }
    });
    let mgmt_stop = {
        let mut stop_rx = stop_rx.clone();
        let mut mgmt_done = mgmt_stop_rx.clone();
        async move {
            // ① 等停机请求(网关面开始收尾)。
            let _ = wait_for_value(&mut stop_rx, true).await;
            // ② 等网关面收尾完成 → 管理面随后收尾。
            //
            // 顺序停机的意义:管理面在网关面收尾期间**继续在线**,浏览器才能
            // 问出「进程还在不在」→ 页面可以如实区分「正在停止」与「已停止」。
            // 若管理面先关,浏览器只看到「连不上」,分不清「进程已退出」与
            // 「正在收尾」。上限兜底:网关面若异常不返回,管理面也必须能关。
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(MGMT_STOP_DEADLINE_SECS),
                wait_for_value(&mut mgmt_done, true),
            )
            .await;
            // 优雅停机开始 → 通知排空计时器(**上限从这里起算**,见 mgmt 段)。
            let _ = mgmt_drain_tx.send(());
        }
    };
    // 20261102_01 P6:网关面改为**独立任务** —— 可暂停 / 恢复,
    // 但整进程停机信号到达时必须一并退出(否则 try_join 永远等不到它)。
    let gateway_task = {
        let control = control.clone();
        let router = gateway_router(shared.clone(), gateway_deps.clone());
        let host = shared.host.clone();
        let port = shared.gateway_port;
        let global_stop = stop_rx.clone();
        tokio::spawn(async move {
            run_gateway_face(router, host, port, gw_listener, control, global_stop).await
        })
    };
    let mgmt_server = axum::serve(
        mgmt_listener,
        // 20261102_01 P1-4:管理面改用带连接信息的 make service,让
        // `/api/wr/session` 能取到 peer addr 做 loopback 判定(D4);
        // **网关面不取 peer,保持原样**(无需连接信息)。
        management_router_with_control(
            shared.clone(),
            gateway_deps.clone(),
            store.clone(),
            control.clone(),
        )
        .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(mgmt_stop)
    .into_future();
    let mut mgmt_server = Box::pin(mgmt_server);
    // 管理面排空**带上限**:axum 的优雅停机会等所有连接结束,长连接不收尾
    // 就永远等不到 → 这里兜底超时,超时后强制结束(日志与库仍会正常收尾)。
    //
    // ⚠️ 上限**只能在优雅停机开始之后**计时(20261104 回归修复):
    // 排空上限的本意是「停机时不被长连接钉死」。若把 `timeout` 直接套在
    // 整个服务生命周期外面,管理面会在**启动后固定时长被强制掐断** ——
    // 监听套接字被释放 → 前端所有管理面请求变成「连不上」(反代回 502),
    // 而网关面与进程却还活着:页面全挂、进程看似正常、停机信号也不起作用。
    // (20261103 的 395280b 引入,20261104 由「页面 502」现场定位。)
    let mgmt_control = control.clone();
    let mgmt = async move {
        let result = tokio::select! {
            // 正常服务中:不设任何时限,管理面活到优雅停机为止。
            result = &mut mgmt_server => result,
            // 优雅停机已开始 → 才开始给排空设上限。
            _ = &mut mgmt_drain_rx => {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(MGMT_DRAIN_TIMEOUT_SECS),
                    &mut mgmt_server,
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => {
                        tracing::warn!(
                            target: "wrouter.cli",
                            "管理面优雅停机超时,强制结束(仍有连接未收尾)"
                        );
                        Ok(())
                    }
                }
            }
        };
        // 管理面**意外**结束(不是停机流程导致的)→ 拉起整进程停机。
        // 管理面是进程对外唯一的管理通道:带着一个死掉的管理面继续跑,
        // 前端只会看到一串 502,而进程看起来一切正常(20261104 现场)。
        // 正常停机收尾时 stop_rx 已是 true → 不触发。
        if !*stop_state_rx.borrow() {
            tracing::error!(
                target: "wrouter.cli",
                "管理面意外结束,触发整进程停机(避免带着死掉的管理面继续运行)"
            );
            mgmt_control.request_stop();
            let _ = stop_tx_guard.send(true);
        }
        result
    };
    let gateway = async move {
        // try_join! 要求两个 future 的错误类型一致(管理面是 io::Error)。
        let result = match gateway_task.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(std::io::Error::other(error)),
            Err(join_error) => Err(std::io::Error::other(join_error)),
        };
        // 网关面**因停机**收尾完成 → 通知管理面收尾(顺序停机的第二段)。
        // 网关面自己意外结束(端口被接管等)→ 不动管理面,保持「管理面继续
        // 服务」的旧语义。
        if *stop_rx.borrow() {
            let _ = mgmt_stop_tx.send(true);
        }
        result
    };
    // 两个面并发收尾;**排空超时已在各自面内部降级为 Ok**,这里只收集真实错误。
    let (mgmt_result, gateway_result) = tokio::join!(mgmt, gateway);
    // 服务已停 → 停机监听任务无意义(避免 sender 被丢弃时误触发)。
    stop_task.abort();
    // 停机收尾(R4.11):日志队列先排空再关闭,维护任务随后 abort;
    // store 的写线程在 drop 前会落一次 wal_checkpoint。
    // **无论两个面是否报错都执行** —— 异常路径下也必须让日志与 WAL 落盘。
    gateway_deps.shutdown_request_logs().await;
    maintenance_task.abort();
    store.close().await;
    mgmt_result?;
    gateway_result?;
    Ok(())
}

/// 等待 watch 值变成 `want`。
///
/// `watch::Receiver::wait_for` 会在 await 期间持有读锁 → 返回的 future
/// **不是 Send**,不能进 `tokio::spawn`(20261102_01 P6 实测踩坑)。
/// 这里改成「`borrow_and_update()` 先判值,不满足再 `changed().await`」,
/// 读锁在 await 之前就释放了 → future 是 Send。
async fn wait_for_value(
    receiver: &mut tokio::sync::watch::Receiver<bool>,
    want: bool,
) -> Result<(), tokio::sync::watch::error::RecvError> {
    loop {
        if *receiver.borrow_and_update() == want {
            return Ok(());
        }
        receiver.changed().await?;
    }
}

/// 网关面任务(20261102_01 P6):可暂停 / 可恢复。
///
/// - **暂停**:收到 `control` 的「停用」意图 → 优雅停机(等 in-flight 请求
///   收尾)→ **释放 3460 监听** → 管理面继续服务(用户仍能看状态/改配置/恢复);
/// - **恢复**:重新 bind 3460 并继续服务。若 bind 失败(端口被别人占走),
///   记 error 状态并重试,**不影响管理面**;
/// - **整进程停机**:`global_stop` 到达 → 与管理面一起优雅退出。
///
/// 首次 bind 在调用方完成(启动即失败 → 整个启动失败,与改造前一致);
/// 本函数只在「恢复」时自己 bind。
async fn run_gateway_face(
    router: axum::Router,
    host: String,
    port: u16,
    initial_listener: tokio::net::TcpListener,
    control: service_control::ServiceControl,
    mut global_stop: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut enabled_rx = control.gateway_enabled_rx();
    // 首轮用调用方已 bind 好的 listener;恢复时自己 bind。
    let mut pending_listener = Some(initial_listener);
    loop {
        // 等待「启用」意图(初始为 true → 直接进入监听)。
        //
        // 暂停期间**必须同时盯着整进程停机信号**:否则「先暂停网关、再点
        // 停止服务」时网关面永远不会退出 → 整个进程永远退不出(端口已关、
        // 进程残留、systemd 仍显示 running)。
        while !*enabled_rx.borrow_and_update() {
            control.set_gateway_running(false);
            tokio::select! {
                changed = enabled_rx.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                }
                stopped = wait_for_value(&mut global_stop, true) => {
                    if stopped.is_err() {
                        return Ok(());
                    }
                    tracing::info!(target: "wrouter.gateway", port, "网关面随进程停机(处于暂停态)");
                    control.set_gateway_running(false);
                    return Ok(());
                }
            }
        }
        let listener = match pending_listener.take() {
            Some(listener) => listener,
            None => match tokio::net::TcpListener::bind((host.as_str(), port)).await {
                Ok(listener) => listener,
                Err(error) => {
                    tracing::warn!(
                        target: "wrouter.gateway",
                        %error,
                        port,
                        "网关面恢复失败(保持暂停,稍后重试)"
                    );
                    control.set_gateway_error(Some(format!("bind {host}:{port} failed: {error}")));
                    control.set_gateway_running(false);
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    continue;
                }
            },
        };
        control.set_gateway_error(None);
        control.set_gateway_running(true);
        let (pause_tx, pause_rx) = tokio::sync::oneshot::channel::<()>();
        // axum 0.8:`with_graceful_shutdown` 返回 IntoFuture,需显式
        // `.into_future()` 才能在 select! 里 await。
        let server = axum::serve(listener, router.clone())
            .with_graceful_shutdown(async move {
                let _ = pause_rx.await;
            })
            .into_future();
        tokio::pin!(server);
        tokio::select! {
            result = &mut server => {
                // 网关面自己结束(极少发生:如端口被外部接管)→ 记错误并退出该面。
                if let Err(error) = result {
                    tracing::warn!(target: "wrouter.gateway", %error, "网关面异常结束");
                    control.set_gateway_error(Some(format!("gateway server error: {error}")));
                }
                control.set_gateway_running(false);
                return Ok(());
            }
            changed = wait_for_value(&mut enabled_rx, false) => {
                if changed.is_err() {
                    break;
                }
                tracing::info!(target: "wrouter.gateway", port, "网关面已暂停(管理面继续服务)");
                // 触发优雅停机 → 等 in-flight 收尾 → 释放监听。
                let _ = pause_tx.send(());
                drain_gateway(&mut server, port).await;
                control.set_gateway_running(false);
            }
            stopped = wait_for_value(&mut global_stop, true) => {
                if stopped.is_err() {
                    break;
                }
                tracing::info!(target: "wrouter.gateway", port, "网关面随进程停机");
                let _ = pause_tx.send(());
                drain_gateway(&mut server, port).await;
                control.set_gateway_running(false);
                return Ok(());
            }
        }
    }
    Ok(())
}

/// 网关面排空:等 in-flight 请求收尾,**带超时上限**。
///
/// 网关转发可能是长流式响应(客户端不会主动断开)→ 没有上限的话,
/// 「暂停 / 停机」会一直等下去,进程退不出。超时后强制结束该面,
/// 管理面与进程收尾不受影响。
async fn drain_gateway<F>(server: &mut F, port: u16)
where
    F: std::future::Future<Output = std::io::Result<()>> + Unpin,
{
    match tokio::time::timeout(
        std::time::Duration::from_secs(GATEWAY_DRAIN_TIMEOUT_SECS),
        &mut *server,
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(target: "wrouter.gateway", %error, port, "网关面收尾结束于错误");
        }
        Err(_) => {
            tracing::warn!(
                target: "wrouter.gateway",
                port,
                "网关面排空超时,强制结束(仍有在途请求)"
            );
        }
    }
}

/// panic 钩子:把 panic 以统一日志格式落 ERROR 后交还默认钩子
/// (保持 stderr 可见性,对齐 uvicorn 的故障可见行为)。
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info.location().map(|l| l.to_string()).unwrap_or_default();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_owned());
        tracing::error!(target: "panic", "panic at {location}: {message}");
        default_hook(info);
    }));
}
