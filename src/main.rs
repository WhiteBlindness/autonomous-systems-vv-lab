use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use autonomous_systems_vv_lab::mission_analysis::{MissionPolicy, analyze_mission};
use autonomous_systems_vv_lab::model::RunStatus;
use autonomous_systems_vv_lab::{
    analyze_artifact, parse_artifact, parse_scenario, replay_artifact, run_scenario,
};

fn main() -> ExitCode {
    match execute_cli(std::env::args().skip(1).collect()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("usage: vv-lab run <scenario.json> --seed <u64> --output <directory>");
            eprintln!("       vv-lab replay <events.json>");
            eprintln!("       vv-lab analyze <events.json> --output <mission.json>");
            eprintln!("       vv-lab benchmark <scenario.json> --seed <u64> --iterations <count>");
            ExitCode::from(1)
        }
    }
}

fn execute_cli(args: Vec<String>) -> Result<u8, String> {
    match args.first().map(String::as_str) {
        Some("run") => run_command(&args[1..]),
        Some("replay") => replay_command(&args[1..]),
        Some("analyze") => analyze_command(&args[1..]),
        Some("benchmark") => benchmark_command(&args[1..]),
        Some("help") | Some("--help") | Some("-h") => {
            print_usage();
            Ok(0)
        }
        Some(command) => Err(format!("unknown command: {command}")),
        None => Err("a command is required".into()),
    }
}

fn run_command(args: &[String]) -> Result<u8, String> {
    let arguments = parse_run_arguments(args)?;
    let scenario_text = read_text_file(&arguments.scenario_path, 2 * 1024 * 1024)?;
    let scenario = parse_scenario(&scenario_text).map_err(|error| error.to_string())?;
    let artifact = run_scenario(scenario, arguments.seed).map_err(|error| error.to_string())?;
    let mission = analyze_mission(
        &artifact.scenario,
        &artifact.expected_report,
        arguments.policy,
    )
    .map_err(|error| error.to_string())?;
    let events_bytes = serde_json::to_vec(&artifact).map_err(|error| error.to_string())?;
    let report_bytes =
        serde_json::to_vec(&artifact.expected_report).map_err(|error| error.to_string())?;
    let mission_bytes = serde_json::to_vec(&mission).map_err(|error| error.to_string())?;
    enforce_output_limit(
        &[&events_bytes, &report_bytes, &mission_bytes],
        arguments.max_output_bytes,
    )?;
    fs::create_dir_all(&arguments.output_directory)
        .map_err(|error| format!("could not create output directory: {error}"))?;
    fs::write(arguments.output_directory.join("events.json"), events_bytes)
        .map_err(|error| format!("could not write events.json: {error}"))?;
    fs::write(arguments.output_directory.join("report.json"), report_bytes)
        .map_err(|error| format!("could not write report.json: {error}"))?;
    fs::write(
        arguments.output_directory.join("mission.json"),
        mission_bytes,
    )
    .map_err(|error| format!("could not write mission.json: {error}"))?;
    println!("run status: {:?}", artifact.expected_report.status);
    Ok(if artifact.expected_report.status == RunStatus::Passed {
        0
    } else {
        2
    })
}

fn analyze_command(args: &[String]) -> Result<u8, String> {
    let arguments = parse_analyze_arguments(args)?;
    let contents = read_text_file(&arguments.events_path, 128 * 1024 * 1024)?;
    let artifact = parse_artifact(&contents).map_err(|error| error.to_string())?;
    let mission =
        analyze_artifact(artifact, arguments.policy).map_err(|error| error.to_string())?;
    let mission_bytes = serde_json::to_vec(&mission).map_err(|error| error.to_string())?;
    enforce_output_limit(&[&mission_bytes], arguments.max_output_bytes)?;
    if let Some(parent) = arguments
        .output_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create output directory: {error}"))?;
    }
    fs::write(&arguments.output_path, mission_bytes)
        .map_err(|error| format!("could not write mission analysis: {error}"))?;
    println!(
        "mission analysis written: {}",
        arguments.output_path.display()
    );
    Ok(0)
}

fn replay_command(args: &[String]) -> Result<u8, String> {
    if args.len() != 1 {
        return Err("replay requires exactly one events.json path".into());
    }
    let contents = read_text_file(&PathBuf::from(&args[0]), 128 * 1024 * 1024)?;
    let artifact = parse_artifact(&contents).map_err(|error| error.to_string())?;
    let outcome = replay_artifact(artifact).map_err(|error| error.to_string())?;
    println!("replay verified: {:?}", outcome.report.status);
    Ok(0)
}

fn benchmark_command(args: &[String]) -> Result<u8, String> {
    let (scenario_path, seed, iterations) = parse_benchmark_arguments(args)?;
    let scenario_text = read_text_file(&scenario_path, 2 * 1024 * 1024)?;
    let scenario = parse_scenario(&scenario_text).map_err(|error| error.to_string())?;
    let warmup = run_scenario(scenario.clone(), seed).map_err(|error| error.to_string())?;
    std::hint::black_box(warmup);

    let started = Instant::now();
    let mut result_hash = String::new();
    for _ in 0..iterations {
        let artifact = run_scenario(scenario.clone(), seed).map_err(|error| error.to_string())?;
        result_hash = artifact.expected_report.final_hash.clone();
        std::hint::black_box(artifact);
    }
    let elapsed_micros = started.elapsed().as_micros();
    let simulated_ticks = u64::from(scenario.steps) * u64::from(iterations);
    println!(
        "{{\"scenario_name\":{},\"seed\":{},\"iterations\":{},\"steps_per_run\":{},\"elapsed_micros\":{},\"simulated_ticks\":{},\"final_hash\":{}}}",
        serde_json::to_string(&scenario.name).map_err(|error| error.to_string())?,
        seed,
        iterations,
        scenario.steps,
        elapsed_micros,
        simulated_ticks,
        serde_json::to_string(&result_hash).map_err(|error| error.to_string())?,
    );
    Ok(0)
}

struct RunArguments {
    scenario_path: PathBuf,
    seed: u64,
    output_directory: PathBuf,
    policy: MissionPolicy,
    max_output_bytes: Option<u64>,
}

struct AnalyzeArguments {
    events_path: PathBuf,
    output_path: PathBuf,
    policy: MissionPolicy,
    max_output_bytes: Option<u64>,
}

fn parse_run_arguments(args: &[String]) -> Result<RunArguments, String> {
    if args.is_empty() {
        return Err("run requires a scenario path".into());
    }
    let scenario_path = PathBuf::from(&args[0]);
    let mut seed = None;
    let mut output = None;
    let mut policy = MissionPolicy::default();
    let mut max_output_bytes = None;
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("missing value after {flag}"))?;
        match flag {
            "--seed" => seed = Some(parse_seed(value)?),
            "--output" => output = Some(PathBuf::from(value)),
            "--stall-window-ticks" => policy.stall_window_ticks = parse_u32(value, flag)?,
            "--min-progress-mm" => policy.min_progress_mm = parse_u32(value, flag)?,
            "--max-output-bytes" => max_output_bytes = Some(parse_u64(value, flag)?),
            _ => return Err(format!("unknown run option: {flag}")),
        }
        index += 2;
    }
    let seed = seed.ok_or_else(|| "--seed is required".to_owned())?;
    let output = output.ok_or_else(|| "--output is required".to_owned())?;
    policy.validate().map_err(|error| error.to_string())?;
    Ok(RunArguments {
        scenario_path,
        seed,
        output_directory: output,
        policy,
        max_output_bytes,
    })
}

fn parse_analyze_arguments(args: &[String]) -> Result<AnalyzeArguments, String> {
    if args.is_empty() {
        return Err("analyze requires an events.json path".into());
    }
    let events_path = PathBuf::from(&args[0]);
    let mut output = None;
    let mut policy = MissionPolicy::default();
    let mut max_output_bytes = None;
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("missing value after {flag}"))?;
        match flag {
            "--output" => output = Some(PathBuf::from(value)),
            "--stall-window-ticks" => policy.stall_window_ticks = parse_u32(value, flag)?,
            "--min-progress-mm" => policy.min_progress_mm = parse_u32(value, flag)?,
            "--max-output-bytes" => max_output_bytes = Some(parse_u64(value, flag)?),
            _ => return Err(format!("unknown analyze option: {flag}")),
        }
        index += 2;
    }
    let output_path = output.ok_or_else(|| "--output is required".to_owned())?;
    policy.validate().map_err(|error| error.to_string())?;
    Ok(AnalyzeArguments {
        events_path,
        output_path,
        policy,
        max_output_bytes,
    })
}

fn parse_benchmark_arguments(args: &[String]) -> Result<(PathBuf, u64, u32), String> {
    if args.is_empty() {
        return Err("benchmark requires a scenario path".into());
    }
    let scenario_path = PathBuf::from(&args[0]);
    let mut seed = None;
    let mut iterations = None;
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("missing value after {flag}"))?;
        match flag {
            "--seed" => seed = Some(parse_seed(value)?),
            "--iterations" => {
                let parsed = value
                    .parse::<u32>()
                    .map_err(|_| "iterations must be a positive integer")?;
                if !(1..=10_000).contains(&parsed) {
                    return Err("iterations must be between 1 and 10000".into());
                }
                iterations = Some(parsed);
            }
            _ => return Err(format!("unknown benchmark option: {flag}")),
        }
        index += 2;
    }
    Ok((
        scenario_path,
        seed.ok_or_else(|| "--seed is required".to_owned())?,
        iterations.ok_or_else(|| "--iterations is required".to_owned())?,
    ))
}

fn parse_seed(value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| "seed must be an unsigned 64-bit integer".into())
}

fn parse_u32(value: &str, label: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| format!("{label} must be an unsigned 32-bit integer"))
}

fn parse_u64(value: &str, label: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("{label} must be an unsigned 64-bit integer"))
}

fn enforce_output_limit(files: &[&[u8]], maximum_bytes: Option<u64>) -> Result<(), String> {
    let Some(maximum_bytes) = maximum_bytes else {
        return Ok(());
    };
    let total_bytes = files
        .iter()
        .try_fold(0_u64, |total, file| total.checked_add(file.len() as u64));
    let total_bytes = total_bytes.ok_or_else(|| "serialized output size overflow".to_owned())?;
    if total_bytes > maximum_bytes {
        return Err(format!(
            "serialized output is {total_bytes} bytes, exceeding --max-output-bytes {maximum_bytes}"
        ));
    }
    Ok(())
}

fn read_text_file(path: &PathBuf, maximum_bytes: usize) -> Result<String, String> {
    let file = fs::File::open(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let mut contents = String::new();
    file.take(maximum_bytes as u64 + 1)
        .read_to_string(&mut contents)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    if contents.len() > maximum_bytes {
        return Err(format!(
            "{} exceeds the configured input size limit",
            path.display()
        ));
    }
    Ok(contents)
}

fn print_usage() {
    println!("vv-lab run <scenario.json> --seed <u64> --output <directory>");
    println!("vv-lab replay <events.json>");
    println!("vv-lab analyze <events.json> --output <mission.json>");
    println!("vv-lab benchmark <scenario.json> --seed <u64> --iterations <count>");
}
