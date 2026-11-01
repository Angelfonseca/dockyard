//! Acciones sobre contenedores (iniciar, detener, reiniciar, borrar) y lectura de su configuración.

use super::*;

impl Engine {
    // ───────────────────────────── Acciones ─────────────────────────────

    pub async fn act(&self, id: &str, act: Act) -> Result<()> {
        match act {
            Act::Start => self.d.start_container(id, None::<StartContainerOptions>).await?,
            Act::Stop => {
                self.d.stop_container(id, Some(StopContainerOptionsBuilder::default().t(15).build())).await?
            }
            Act::Restart => {
                self.d.restart_container(id, Some(RestartContainerOptionsBuilder::default().t(15).build())).await?
            }
            Act::Kill => {
                self.d.kill_container(id, Some(KillContainerOptionsBuilder::default().signal("SIGKILL").build())).await?
            }
            Act::Pause => self.d.pause_container(id).await?,
            Act::Unpause => self.d.unpause_container(id).await?,
            Act::Remove => {
                self.d
                    .remove_container(id, Some(RemoveContainerOptionsBuilder::default().force(true).v(false).build()))
                    .await?
            }
        }
        Ok(())
    }

    pub async fn inspect_text(&self, id: &str) -> Result<String> {
        let i = self.d.inspect_container(id, None::<InspectContainerOptions>).await?;
        Ok(serde_json::to_string_pretty(&i)?)
    }

    pub fn log_stream(&self, id: &str, tail: &str, follow: bool, timestamps: bool) -> impl Stream<Item = Result<LogOutput, bollard::errors::Error>> + use<> {
        let opts = LogsOptionsBuilder::default().follow(follow).stdout(true).stderr(true).timestamps(timestamps).tail(tail).build();
        self.d.logs(id, Some(opts))
    }

}
