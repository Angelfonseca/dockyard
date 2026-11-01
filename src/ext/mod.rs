//! Programas externos opcionales: docker compose, git, docker build, el editor y comandos del host.
//! Todo lo esencial va por la API; esto solo se usa si el programa existe, y si falta se dice qué instalar.

use crate::catalog::{Plan, Template};
use crate::engine::{Engine, Prog, UpdateOutcome};
use crate::util::*;
use anyhow::{Context, Result, anyhow, bail};
use bollard::query_parameters::*;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use tokio::io::{AsyncBufReadExt, BufReader};

pub type Out<'a> = &'a mut (dyn FnMut(String) + Send);

pub const COMPOSE_FILES: [&str; 4] = ["compose.yaml", "compose.yml", "docker-compose.yml", "docker-compose.yaml"];

mod detect;
mod run;
mod compose;
mod git;
mod git_build;
mod envfile;
mod update;
mod host;
#[cfg(test)]
mod tests;

pub use compose::*;
pub use detect::*;
pub use envfile::*;
pub use git::*;
pub use git_build::*;
pub use host::*;
pub use run::*;
pub use update::*;
