/* Reduce runs to the fan-in limit with parallel intermediate merges.
The remaining runs are consumed directly by the final compressed writer. */
use super::merge_runs;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

// Leave headroom for output files, stdio and other process descriptors.
pub const MAX_OPEN_INPUTS: usize = 128;

pub fn prepare_runs(
    mut runs: Vec<PathBuf>,
    work: &Path,
    fan_in: usize,
    workers: usize,
    keep_intermediate: bool,
) -> io::Result<Vec<PathBuf>> {
    if !(2..=MAX_OPEN_INPUTS).contains(&fan_in) || workers == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Merge fan-in must be 2..=128 and workers must be positive",
        ));
    }

    let mut pass = 0;
    while runs.len() > fan_in {
        pass += 1;
        let directory = work.join(format!("pass_{pass:03}"));
        fs::create_dir(&directory)?;
        let groups: Vec<_> = runs.chunks(fan_in).collect();
        let next: Vec<_> = groups
            .iter()
            .enumerate()
            .map(|(number, group)| {
                if group.len() == 1 {
                    group[0].clone()
                } else {
                    directory.join(format!("run_{:04}.bin", number + 1))
                }
            })
            .collect();
        let active_workers = workers.min(groups.len()).min(MAX_OPEN_INPUTS / fan_in);
        let next_group = AtomicUsize::new(0);
        let cancelled = AtomicBool::new(false);

        thread::scope(|scope| -> io::Result<()> {
            let mut handles = Vec::new();
            for _ in 0..active_workers {
                handles.push(scope.spawn(|| -> io::Result<()> {
                    while !cancelled.load(Ordering::Relaxed) {
                        let number = next_group.fetch_add(1, Ordering::Relaxed);
                        let Some(group) = groups.get(number) else {
                            break;
                        };
                        if group.len() == 1 {
                            continue;
                        }
                        let result = merge_runs(group, &next[number]).and_then(|_| {
                            if !keep_intermediate {
                                for input in *group {
                                    fs::remove_file(input)?;
                                }
                            }
                            Ok(())
                        });
                        if let Err(error) = result {
                            cancelled.store(true, Ordering::Relaxed);
                            return Err(error);
                        }
                    }
                    Ok(())
                }));
            }
            let mut first_error = None;
            for handle in handles {
                let result = handle.join().unwrap_or_else(|_| {
                    cancelled.store(true, Ordering::Relaxed);
                    Err(io::Error::other("Merge worker panicked"))
                });
                if let Err(error) = result {
                    first_error.get_or_insert(error);
                }
            }
            match first_error {
                Some(error) => Err(error),
                None => Ok(()),
            }
        })?;

        println!(
            "Intermediate merge pass {pass}: {} inputs -> {} outputs ({active_workers} workers)",
            runs.len(),
            next.len()
        );
        runs = next;
    }
    Ok(runs)
}
