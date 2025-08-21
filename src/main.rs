use std::fs::{create_dir_all, File};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Command as ProcessCommand;
use clap::{Arg, Command};
use serde_json;
use tempfile::NamedTempFile;

const CONFIG_DIR: &str = "/etc/hacker-ostree";
const REPOS_FILE: &str = "/etc/hacker-ostree/repos.json";
const VAR_DIR: &str = "/var/lib/hacker-ostree";
const CACHE_DIR: &str = "/var/lib/hacker-ostree/apt-cache";
const OVERLAY_DIR: &str = "/var/lib/hacker-ostree/overlay";
const INSTALLED_PKGS_FILE: &str = "/var/lib/hacker-ostree/installed_packages.txt";

// Helper function to run shell commands
fn run_command(cmd: &str, args: &[&str]) -> Result<String, String> {
    let output = ProcessCommand::new(cmd)
    .args(args)
    .output()
    .map_err(|e| format!("Failed to execute {}: {}", cmd, e))?;

    if !output.status.success() {
        return Err(format!(
            "Command failed: {}\nStderr: {}",
            cmd,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// Ensure directories exist
fn ensure_dirs() -> Result<(), String> {
    create_dir_all(CONFIG_DIR).map_err(|e| format!("Failed to create {}: {}", CONFIG_DIR, e))?;
    create_dir_all(VAR_DIR).map_err(|e| format!("Failed to create {}: {}", VAR_DIR, e))?;
    create_dir_all(CACHE_DIR).map_err(|e| format!("Failed to create {}: {}", CACHE_DIR, e))?;
    create_dir_all(OVERLAY_DIR).map_err(|e| format!("Failed to create {}: {}", OVERLAY_DIR, e))?;
    Ok(())
}

// Load repos from repos.json
fn load_repos() -> Result<Vec<String>, String> {
    let path = Path::new(REPOS_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(path).map_err(|e| format!("Failed to open {}: {}", REPOS_FILE, e))?;
    let repos: Vec<String> = serde_json::from_reader(file).map_err(|e| format!("Failed to parse {}: {}", REPOS_FILE, e))?;
    Ok(repos)
}

// Save repos to repos.json
fn save_repos(repos: &[String]) -> Result<(), String> {
    let file = File::create(REPOS_FILE).map_err(|e| format!("Failed to create {}: {}", REPOS_FILE, e))?;
    serde_json::to_writer_pretty(file, repos).map_err(|e| format!("Failed to write to {}: {}", REPOS_FILE, e))?;
    Ok(())
}

// Create temporary sources.list from repos
fn create_temp_sources_list() -> Result<NamedTempFile, String> {
    let repos = load_repos()?;
    let mut temp_file = NamedTempFile::new().map_err(|e| format!("Failed to create temp file: {}", e))?;
    for repo in repos {
        writeln!(temp_file, "{}", repo).map_err(|e| format!("Failed to write to temp file: {}", e))?;
    }
    Ok(temp_file)
}

// Function to update APT cache using custom sources
fn apt_update() -> Result<(), String> {
    ensure_dirs()?;
    let temp_sources = create_temp_sources_list()?;
    let sources_path = temp_sources.path().to_str().ok_or_else(|| "Failed to get temp file path".to_string())?;
    let cache_dir = format!("Dir::Cache={}", CACHE_DIR);
    let source_list = format!("Dir::Etc::SourceList={}", sources_path);

    let update_args = vec![
        "update",
        "-o", &cache_dir,
        "-o", &source_list,
        "-o", "Dir::Etc::SourceParts=-", // Disable source parts
    ];
    run_command("apt-get", &update_args)?;
    Ok(())
}

// Function to install a package
fn install_package(package: &str) -> Result<(), String> {
    ensure_dirs()?;
    apt_update()?; // Ensure cache is updated

    let temp_sources = create_temp_sources_list()?;
    let sources_path = temp_sources.path().to_str().ok_or_else(|| "Failed to get temp file path".to_string())?;
    let cache_dir = format!("Dir::Cache={}", CACHE_DIR);
    let source_list = format!("Dir::Etc::SourceList={}", sources_path);

    // Download package
    let download_args = vec![
        "download",
        package,
        "-o", &cache_dir,
        "-o", &source_list,
        "-o", "Dir::Etc::SourceParts=-",
    ];
    run_command("apt-get", &download_args)?;

    // Find the downloaded .deb file
    let deb_pattern = format!("{}/{}_*.deb", CACHE_DIR, package);
    let ls_output = run_command("ls", &[&deb_pattern])?;
    let deb_files: Vec<&str> = ls_output.trim().split('\n').collect();
    if deb_files.is_empty() || deb_files[0].is_empty() {
        return Err(format!("No .deb file found for {}", package));
    }
    let deb_path = deb_files[0];

    // Install to overlay
    let install_args = vec![
        "--instdir",
        OVERLAY_DIR,
        "--force-not-root",
        "--force-overwrite",
        "-i",
        deb_path,
    ];
    run_command("dpkg", &install_args)?;

    // Record installed package if not already there
    let mut installed = load_installed_packages()?;
    if !installed.contains(&package.to_string()) {
        installed.push(package.to_string());
        save_installed_packages(&installed)?;
    }

    Ok(())
}

// Function to remove a package
fn remove_package(package: &str) -> Result<(), String> {
    // Remove from overlay
    let remove_args = vec![
        "--instdir",
        OVERLAY_DIR,
        "--force-not-root",
        "-r",
        package,
    ];
    run_command("dpkg", &remove_args)?;

    // Remove from installed list
    let mut installed = load_installed_packages()?;
    installed.retain(|p| p != package);
    save_installed_packages(&installed)?;

    Ok(())
}

// Function to list installed packages
fn list_packages() -> Result<Vec<String>, String> {
    load_installed_packages()
}

// Function to search packages in APT
fn search_package(query: &str) -> Result<String, String> {
    let temp_sources = create_temp_sources_list()?;
    let sources_path = temp_sources.path().to_str().ok_or_else(|| "Failed to get temp file path".to_string())?;
    let source_list = format!("Dir::Etc::SourceList={}", sources_path);

    let search_args = vec![
        "search",
        "-o", &source_list,
        "-o", "Dir::Etc::SourceParts=-",
        query,
    ];
    run_command("apt-cache", &search_args)
}

// Function to upgrade all installed packages in overlay
fn upgrade_packages() -> Result<(), String> {
    apt_update()?;
    let installed = load_installed_packages()?;
    for pkg in installed {
        install_package(&pkg)?;
    }
    Ok(())
}

// Function to update system (OSTree pull and deploy)
fn system_update() -> Result<(), String> {
    // Assuming OSTree remote 'origin' and ref 'main'
    run_command("ostree", &["pull", "origin", "main"])?;

    // Deploy the new commit
    run_command("ostree", &["admin", "deploy", "origin:main"])?;

    // Resync overlay
    resync_overlay()?;

    Ok(())
}

// Function to rollback
fn rollback() -> Result<(), String> {
    run_command("ostree", &["admin", "undeploy", "0"])?;
    Ok(())
}

// Function to resync overlay after rootfs update
fn resync_overlay() -> Result<(), String> {
    let installed = load_installed_packages()?;
    for pkg in installed {
        install_package(&pkg)?;
    }
    Ok(())
}

// Load installed packages from file
fn load_installed_packages() -> Result<Vec<String>, String> {
    let path = Path::new(INSTALLED_PKGS_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(path).map_err(|e| format!("Failed to open {}: {}", INSTALLED_PKGS_FILE, e))?;
    let reader = BufReader::new(file);
    let mut packages = Vec::new();
    for line in reader.lines() {
        let pkg = line.map_err(|e| format!("Failed to read line: {}", e))?.trim().to_string();
        if !pkg.is_empty() {
            packages.push(pkg);
        }
    }
    Ok(packages)
}

// Save installed packages to file
fn save_installed_packages(packages: &[String]) -> Result<(), String> {
    let mut file = File::create(INSTALLED_PKGS_FILE).map_err(|e| format!("Failed to create {}: {}", INSTALLED_PKGS_FILE, e))?;
    for pkg in packages {
        writeln!(file, "{}", pkg).map_err(|e| format!("Failed to write to {}: {}", INSTALLED_PKGS_FILE, e))?;
    }
    Ok(())
}

// Function to clean cache
fn clean_cache() -> Result<(), String> {
    run_command("rm", &["-rf", &format!("{}/archives/*", CACHE_DIR)])?;
    Ok(())
}

// Function to add repo
fn add_repo(repo_line: &str) -> Result<(), String> {
    let mut repos = load_repos()?;
    repos.push(repo_line.to_string());
    save_repos(&repos)?;
    Ok(())
}

// Function to remove repo
fn remove_repo(index: usize) -> Result<(), String> {
    let mut repos = load_repos()?;
    if index < repos.len() {
        repos.remove(index);
        save_repos(&repos)?;
        Ok(())
    } else {
        Err("Invalid index".to_string())
    }
}

// Function to list repos
fn list_repos() -> Result<Vec<String>, String> {
    load_repos()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let matches = Command::new("hacker-ostree")
    .version("0.3.0")
    .author("Your Name")
    .about("Custom package manager for atomic systems with APT overlay")
    .subcommand(Command::new("update")
    .about("Update APT cache"))
    .subcommand(Command::new("upgrade")
    .about("Upgrade all installed packages in overlay"))
    .subcommand(Command::new("system-update")
    .about("Update the system via OSTree pull and deploy"))
    .subcommand(Command::new("system-upgrade")
    .about("Alias for system-update"))
    .subcommand(Command::new("install")
    .about("Install a DEB package to overlay")
    .arg(Arg::new("PACKAGE")
    .required(true)
    .index(1)))
    .subcommand(Command::new("remove")
    .about("Remove a DEB package from overlay")
    .arg(Arg::new("PACKAGE")
    .required(true)
    .index(1)))
    .subcommand(Command::new("list")
    .about("List installed packages"))
    .subcommand(Command::new("search")
    .about("Search for packages in APT repositories")
    .arg(Arg::new("QUERY")
    .required(true)
    .index(1)))
    .subcommand(Command::new("rollback")
    .about("Rollback to previous OSTree commit"))
    .subcommand(Command::new("resync")
    .about("Resync overlay with installed packages"))
    .subcommand(Command::new("clean")
    .about("Clean APT cache"))
    .subcommand(Command::new("repo")
    .about("Manage repositories")
    .subcommand(Command::new("list")
    .about("List repositories"))
    .subcommand(Command::new("add")
    .about("Add a repository")
    .arg(Arg::new("REPO_LINE")
    .required(true)
    .index(1)))
    .subcommand(Command::new("remove")
    .about("Remove a repository by index")
    .arg(Arg::new("INDEX")
    .required(true)
    .index(1))))
    .get_matches();

    match matches.subcommand() {
        Some(("update", _)) => apt_update()?,
        Some(("upgrade", _)) => upgrade_packages()?,
        Some(("system-update", _)) | Some(("system-upgrade", _)) => system_update()?,
        Some(("install", sub_m)) => install_package(sub_m.get_one::<String>("PACKAGE").unwrap())?,
        Some(("remove", sub_m)) => remove_package(sub_m.get_one::<String>("PACKAGE").unwrap())?,
        Some(("list", _)) => {
            let pkgs = list_packages()?;
            println!("Installed packages:");
            for pkg in pkgs {
                println!("- {}", pkg);
            }
        }
        Some(("search", sub_m)) => {
            let output = search_package(sub_m.get_one::<String>("QUERY").unwrap())?;
            print!("{}", output);
        }
        Some(("rollback", _)) => rollback()?,
        Some(("resync", _)) => resync_overlay()?,
        Some(("clean", _)) => clean_cache()?,
        Some(("repo", sub_m)) => match sub_m.subcommand() {
            Some(("list", _)) => {
                let repos = list_repos()?;
                println!("Repositories:");
                for (i, repo) in repos.iter().enumerate() {
                    println!("{}: {}", i, repo);
                }
            }
            Some(("add", add_m)) => add_repo(add_m.get_one::<String>("REPO_LINE").unwrap())?,
            Some(("remove", rm_m)) => {
                let index: usize = rm_m.get_one::<String>("INDEX").unwrap().parse()?;
                remove_repo(index)?;
            }
            _ => println!("Invalid repo subcommand"),
        },
        _ => {
            println!("Usage: hacker-ostree <COMMAND>\n");
            println!("Commands:");
            println!("  update          Update APT cache");
            println!("  upgrade         Upgrade all installed packages in overlay");
            println!("  system-update   Update the system via OSTree pull and deploy");
            println!("  system-upgrade  Alias for system-update");
            println!("  install         Install a DEB package to overlay");
            println!("  remove          Remove a DEB package from overlay");
            println!("  list            List installed packages");
            println!("  search          Search for packages in APT repositories");
            println!("  rollback        Rollback to previous OSTree commit");
            println!("  resync          Resync overlay with installed packages");
            println!("  clean           Clean APT cache");
            println!("  repo list       List repositories");
            println!("  repo add        Add a repository");
            println!("  repo remove     Remove a repository by index");
        }
    }

    Ok(())
}
