//! Explicit, opt-in live test. Never invoked by cargo test or normal Hub startup.
use clap::Parser;
use nagisalake_agent::{
    AgentService, RunEvent, RunRequest, RuntimeConfig, Skill,
    opencode::{OpenCode, OpenCodeConfig},
};
use serde::Deserialize;
use service_async::Service;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "examples/nagisalake-hub.toml")]
    config:      PathBuf,
    #[arg(long)]
    skill:       Option<String>,
    #[arg(long)]
    input:       Option<String>,
    #[arg(long)]
    list_skills: bool,
}
#[derive(Deserialize)]
struct Config {
    opencode: Settings,
}
#[derive(Deserialize)]
struct Settings {
    #[serde(flatten)]
    backend:        OpenCodeConfig,
    #[serde(flatten)]
    runtime:        RuntimeConfig,
    #[serde(default)]
    allowed_skills: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("nagisalake_agent=info")
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    let config: Config = toml::from_str(&std::fs::read_to_string(args.config)?)?;
    let backend = OpenCode::new(config.opencode.backend)?;
    let health = backend.client().health().await?;
    eprintln!(
        "OpenCode healthy={}, version={}",
        health.healthy, health.version
    );
    if !health.healthy {
        return Err("OpenCode is unhealthy".into());
    }
    if args.list_skills {
        for skill in backend.client().skills().await? {
            println!("{}", skill.name);
        }
        return Ok(());
    }
    let skills = config
        .opencode
        .allowed_skills
        .into_iter()
        .map(|id| Skill {
            id,
            version: "1".into(),
        })
        .collect();
    let service = AgentService::new(backend.clone(), skills, config.opencode.runtime)?;
    let skill = args
        .skill
        .ok_or("pass --skill with a configured allowed skill name")?;
    let input = args
        .input
        .ok_or("pass --input (this opt-in test calls the configured LLM provider)")?;
    let mut run = service
        .call(RunRequest {
            skill,
            input,
            options: BTreeMap::new(),
        })
        .await?;
    let mut completed = false;
    while let Some(event) = run.recv().await {
        println!("{}", serde_json::to_string(&event)?);
        if matches!(event, RunEvent::Completed { .. }) {
            completed = true;
        }
    }
    backend.client().wait_for_cleanup().await;
    if !completed {
        return Err("agent execution did not complete successfully".into());
    }
    Ok(())
}
