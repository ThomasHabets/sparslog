use anyhow::Result;
use clap::Parser;
use rustradio::graph::GraphRunner;

mod sparslog;

#[tokio::main]
async fn main() -> Result<()> {
    println!(
        "Sparslog (async) {} ({})",
        sparslog::VERSION,
        env!("GIT_VERSION")
    );

    #[cfg(feature = "tokio-unstable")]
    console_subscriber::init();

    let opt = sparslog::Opt::parse();
    stderrlog::new()
        .module(module_path!())
        .module("rustradio")
        .quiet(false)
        .verbosity(opt.verbose)
        .timestamp(stderrlog::Timestamp::Second)
        .init()
        .unwrap();

    let mut graph = rustradio::agraph::AsyncGraph::new();

    let iq_listener = sparslog::create_graph(&mut graph, &opt)?;

    // Set up to run.
    let cancel = graph.cancel_token();
    ctrlc::set_handler(move || {
        eprintln!("Received Ctrl+C!");
        cancel.cancel();
    })
    .expect("Error setting Ctrl-C handler");

    // Run.
    eprintln!("Running…");
    let result = graph.run_async().await;
    let iq_result = iq_listener.map(sparslog::IqListener::shutdown).transpose();
    result?;
    iq_result?;
    eprintln!("{}", graph.generate_stats().unwrap_or("No stats".into()));
    Ok(())
}
/* vim: textwidth=80
 */
