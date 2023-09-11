use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

use anyhow::{Context, Result};
use hiercmd::{args, Level};

use crate::Stuff;

struct Options {
    dry_run: bool,
    verbose: bool,
}

pub async fn upload(mut l: Level<Stuff>) -> Result<()> {
    l.optflag("", "dry-run", "print what you would do instead of doing it");
    l.optflag("v", "verbose", "print out more info about what is going on");

    let a = args!(l);
    let _c = l.context().user();

    let options = Options {
        dry_run: a.opts().opt_present("dry-run"),
        verbose: a.opts().opt_present("verbose"),
    };

    println!("cache upload");

    let current_dir = std::env::current_dir()?;

    clean_target_dir(&options, &current_dir)?;

    let hash = calculate_hash(&options, &current_dir)?;
    let hash = hash_to_string(hash);

    if options.dry_run {
        println!("hash: {hash}");
    }

    Ok(())
}

pub async fn restore(mut l: Level<Stuff>) -> Result<()> {
    let _a = args!(l);

    let _c = l.context().user();

    println!("cache restore");

    Ok(())
}

/// Removes stuff we don't want from the target directory.
/// 
/// Before we save up our cache, we want to remove some files we don't actually
/// want to save. We want to save the cache of artifacts for our dependencies only,
/// and not for our own code, as that's going to be changing on every job, and
/// so saving it doesn't make much sense.
/// 
/// This function figures out what stuff can be removed, and what stuff should
/// stay. It is loosely based on some code from `Swatinem/rust-cache`, and some
/// code from rust-analyzer before it moved to `rust-cache`.
fn clean_target_dir<P: Into<PathBuf>>(
    options: &Options,
    base_dir: P,
) -> Result<()> {
    let base_dir = base_dir.into();
    let cargo_toml = base_dir.join("Cargo.toml");

    // get the target directory
    let mut cmd = cargo_metadata::MetadataCommand::new();
    cmd.manifest_path(&cargo_toml);
    let metadata = cmd.exec()?;
    let target_dir = &metadata.target_directory;

    if options.verbose {
        println!("cleaning target directory '{target_dir}'");
    }

    // first, we don't need this file
    let rustc_info = target_dir.join(".rustc_info.json");
    std::fs::remove_file(rustc_info)
        .context("failed to remove .rustc_info.json")?;

    // we want to clean up our own files, but leave the ones for our dependencies
    let mut to_delete = Vec::new();

    for package in metadata.workspace_packages() {
        let package_name = package.name.replace("-", "_");

        if options.verbose {
            println!("cleaning package: {}", package_name);
        }

        to_delete.push(package_name);
    }

    // these two directories contain the things we want to get rid of
    let dirs =
        [target_dir.join("debug/deps"), target_dir.join("debug/.fingerprint")];

    for dir in dirs.iter() {
        for path in read_dir(dir)? {
            // we want to display this in multiple log lines and error messages,
            // so let's just do it once here.
            let file_to_display = path.display();

            if options.verbose {
                println!("considering {}", file_to_display);
            }

            let filename =
                path.file_name().context("has no file name")?.to_string_lossy();

            let (stem, _) = match rsplit_once(&filename, '-') {
                Some(it) => it,
                None => {
                    if options.verbose {
                        println!("deleting: {}", file_to_display);
                    }
                    if !options.dry_run {
                        rm_rf(&path).with_context(|| {
                            format!("failed to remove {}", file_to_display)
                        })?;
                    }
                    continue;
                }
            };

            let stem = stem.replace('-', "_");
            if to_delete.contains(&stem) {
                if options.verbose {
                    println!("deleting file: {}", file_to_display);
                }
                if !options.dry_run {
                    rm_rf(&path).with_context(|| {
                        format!("failed to remove {}", file_to_display)
                    })?;
                }
            }

            if options.verbose {
                println!("did not delete: {}", file_to_display);
            }
        }
    }

    Ok(())
}

// recursively read the files from a directory
fn read_dir(path: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
    let mut res = Vec::new();
    for entry in path.read_dir()? {
        let entry = entry?;
        res.push(entry.path())
    }
    Ok(res)
}

/// a helpful utility for getting what we want out of a filename
/// 
/// we have files that look like this:
/// 
/// * `/target/debug/deps/xtask-ca8ab49bcd07610e.d`
/// * `/target/debug/.fingerprint/xtask-b6a1d8acea55233f`
/// 
/// we'll want to test these against `xtask` to know if we should get rid of
/// them. so this function returns
/// 
/// * ("xtask", "ca8ab49bcd07610e.d")
/// * ("xtask", "b6a1d8acea55233f")
///
/// for each of these, making it easy to compare against the returned stem.
fn rsplit_once(haystack: &str, delim: char) -> Option<(&str, &str)> {
    let mut split = haystack.rsplitn(2, delim);
    let suffix = split.next()?;
    let prefix = split.next()?;
    Some((prefix, suffix))
}

fn rm_rf(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let metadata = path.metadata()?;

    if metadata.is_dir() {
        std::fs::remove_dir_all(&path)?
    } else {
        std::fs::remove_file(&path)?
    }

    Ok(())
}

fn calculate_hash<P: AsRef<Path>>(
    options: &Options,
    base_dir: P,
) -> Result<[u8; 32]> {
    if options.verbose {
        println!(
            "calculating hash for directory '{}'",
            base_dir.as_ref().display()
        );
    }

    let mut hash = hmac_sha256::Hash::new();

    // TODO: Consider including some sort of per-job ID into the hash

    let rustinfo = Command::new("rustc").arg("-vV").output()?.stdout;
    let rustinfo = String::from_utf8(rustinfo)?;

    let wanted_info = ["host: ", "release: ", "commit-hash: "];

    for line in rustinfo.lines() {
        for info in wanted_info {
            if line.starts_with(info) {
                let mut iter = line.split(info);
                // throw away the prefix...
                iter.next();
                // ... and grab our info
                let value = iter.next().unwrap();

                // then include it in our hash
                hash.update(value);

                if options.verbose {
                    println!("including {info}{value} in hash")
                }
            }
        }
    }

    // we want to save the value of environment variables with these prefixes
    let env_prefixes = ["CARGO", "CC", "CFLAGS", "CXX", "CMAKE", "RUST"];

    // btreemap is chosen because it is ordered, to provide stability for our hash
    let mut envs: BTreeMap<String, String> = BTreeMap::new();

    // find all env keys that have our prefixes...
    for (key, value) in env::vars() {
        for prefix in env_prefixes {
            if key.starts_with(prefix) {
                envs.insert(key.clone(), value.clone());
            }
        }
    }

    // ... and put them into our hash
    for (key, value) in envs {
        if options.verbose {
            println!("including {key}={value} in hash");
        }

        hash.update(format!("{}={}", key, value));
    }

    let mut files = Vec::new();

    // we also want to include the contents of these files
    let globs = [
        "**/.cargo/config.toml",
        "**/rust-toolchain",
        "**/rust-toolchain.toml",
        "**/Cargo.toml",
        "**/Cargo.lock",
    ];

    for glob in globs {
        for entry in glob::glob(glob).expect("failed to read glob pattern") {
            // if we don't find anything, that's okay
            if let Ok(path) = entry {
                files.push(path);
            }
        }
    }

    files.dedup();
    files.sort();

    // append all files to the hash
    for file in files {
        if options.verbose {
            println!("including contents of {} in hash", file.display());
        }

        let contents = fs::read(file)?;
        hash.update(contents);
    }

    // done!
    Ok(hash.finalize())
}

fn hash_to_string(input: [u8; 32]) -> String {
    let mut s = String::new();
    for byte in input {
        write!(&mut s, "{:x}", byte).expect("Unable to write");
    }
    s
}
