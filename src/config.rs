//! Configuración opcional en ~/.config/dockyard/config.toml

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Config {
    /// Zona horaria que se inyecta como TZ en los contenedores nuevos (por defecto: la del sistema).
    pub tz: Option<String>,
    /// Carpeta de respaldos (por defecto ~/dockyard-backups).
    pub backups_dir: Option<String>,
    /// Segundos entre actualizaciones del panel (por defecto 2).
    pub refresh_secs: Option<u64>,
    /// Política de reinicio por defecto en despliegues nuevos.
    pub default_restart: Option<String>,
    /// Tamaño máximo de cada archivo de log de los contenedores (por defecto "10m"; "" = no limitar).
    pub log_max_size: Option<String>,
    /// Cuántos archivos de log rotados conservar (por defecto 3).
    pub log_max_file: Option<u32>,
}

impl Config {
    pub fn load() -> (Config, Option<String>) {
        let path = crate::util::config_dir().join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(mut c) => {
                    if c.tz.is_none() {
                        c.tz = crate::util::detect_tz();
                    }
                    (c, None)
                }
                Err(e) => (Config { tz: crate::util::detect_tz(), ..Default::default() }, Some(format!("{}: {e}", path.display()))),
            },
            Err(_) => (Config { tz: crate::util::detect_tz(), ..Default::default() }, None),
        }
    }

    pub fn backups_dir(&self) -> PathBuf {
        match &self.backups_dir {
            Some(d) if !d.is_empty() => PathBuf::from(d),
            _ => crate::util::backups_dir(),
        }
    }

    pub fn refresh_ms(&self) -> u64 {
        self.refresh_secs.unwrap_or(2).clamp(1, 60) * 1000
    }

    pub fn log_rotation(&self) -> Option<(String, u32)> {
        let size = self.log_max_size.clone().unwrap_or_else(|| "10m".into());
        if size.trim().is_empty() {
            return None;
        }
        Some((size.trim().to_string(), self.log_max_file.unwrap_or(3)))
    }

    pub fn restart(&self) -> String {
        match &self.default_restart {
            Some(r) if crate::catalog::RESTARTS.contains(&r.as_str()) => r.clone(),
            _ => "unless-stopped".into(),
        }
    }
}
