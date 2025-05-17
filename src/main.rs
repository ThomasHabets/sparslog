use anyhow::Result;
use clap::Parser;
use rustradio::graph::GraphRunner;

mod sparslog;

fn main() -> Result<()> {
    println!("Sparslog (sync)");
    let opt = sparslog::Opt::parse();
    stderrlog::new()
        .module(module_path!())
        .module("rustradio")
        .quiet(false)
        .verbosity(opt.verbose)
        .timestamp(stderrlog::Timestamp::Second)
        .init()
        .unwrap();

    let mut graph: Box<dyn GraphRunner> = if opt.multithread {
        Box::new(rustradio::mtgraph::MTGraph::new())
    } else {
        Box::new(rustradio::graph::Graph::new())
    };
    sparslog::create_graph(&mut *graph, &opt)?;

    // Set up to run.
    let cancel = graph.cancel_token();
    ctrlc::set_handler(move || {
        eprintln!("Received Ctrl+C!");
        cancel.cancel();
    })
    .expect("Error setting Ctrl-C handler");

    // Run.
    eprintln!("Running…");
    graph.run()?;
    eprintln!("{}", graph.generate_stats().unwrap());
    Ok(())
}
/* vim: textwidth=80
 */
