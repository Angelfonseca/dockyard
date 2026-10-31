//! Trabajos en segundo plano (instalar, actualizar, respaldar…) que la página consulta.

use super::*;

// ───────────────────────────── Trabajos ─────────────────────────────

/// Las líneas de progreso que se repiten («↓ imagen 45%», «Esperando… 3 s») reemplazan a la anterior.
pub(super) fn job_push(ctx: &Ctx, id: u64, line: String) {
    let mut j = ctx.jobs.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(job) = j.1.get_mut(&id) else { return };
    let shape = |s: &str| s.chars().filter(|c| !c.is_ascii_digit()).collect::<String>();
    let repeats = ["↓", "↑", "Esperando"].iter().any(|p| line.starts_with(p));
    match job.lines.last_mut() {
        Some(last) if repeats && shape(last) == shape(&line) => *last = line,
        Some(last) if repeats && line.starts_with('↓') && last.starts_with('↓') && last.split("  ").next() == line.split("  ").next() => *last = line,
        _ => {
            job.lines.push(line);
            if job.lines.len() > 400 {
                job.lines.remove(0);
            }
        }
    }
}

pub(super) fn progress(ctx: &Arc<Ctx>, id: u64) -> impl FnMut(String) + Send + use<> {
    let c = ctx.clone();
    move |s| job_push(&c, id, s)
}

pub(super) fn spawn_job<Fut>(ctx: &Arc<Ctx>, title: String, f: impl FnOnce(Arc<Ctx>, u64) -> Fut) -> Resp
where
    Fut: Future<Output = Result<Value>> + Send + 'static,
{
    let id = {
        let mut j = ctx.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        j.0 += 1;
        let id = j.0;
        j.1.insert(id, Job { title, lines: Vec::new(), done: false, ok: false, result: Value::Null });
        while j.1.len() > 50 {
            let oldest = *j.1.keys().next().unwrap();
            j.1.remove(&oldest);
        }
        id
    };
    let fut = f(ctx.clone(), id);
    let c = ctx.clone();
    tokio::spawn(async move {
        let r = fut.await;
        c.shared.notify.notify_one();
        c.shared.usage_now.notify_one();
        let mut j = c.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(job) = j.1.get_mut(&id) {
            job.done = true;
            match r {
                Ok(v) => {
                    job.ok = true;
                    job.result = v;
                }
                Err(e) => job.result = json!({ "error": format!("{e:#}") }),
            }
        }
    });
    json_resp(json!({ "job": id }))
}

