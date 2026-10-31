//! Clasificación de errores para la CLI: cada tipo tiene su código de salida, así un script puede distinguirlos.

use std::fmt;

/// El daemon no respondió a tiempo (ver `engine::timed`).
#[derive(Debug)]
pub struct Timeout(pub u64);

impl fmt::Display for Timeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Docker no responde (más de {} s)", self.0)
    }
}

impl std::error::Error for Timeout {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Other,
    NotFound,
    Conflict,
    Unavailable,
}

impl Kind {
    /// 0 éxito · 1 otro error · 2 no existe · 3 conflicto · 4 daemon no disponible o sin respuesta.
    pub fn exit_code(self) -> i32 {
        match self {
            Kind::Other => 1,
            Kind::NotFound => 2,
            Kind::Conflict => 3,
            Kind::Unavailable => 4,
        }
    }
}

pub fn kind(e: &anyhow::Error) -> Kind {
    if e.downcast_ref::<Timeout>().is_some() {
        return Kind::Unavailable;
    }
    use bollard::errors::Error as B;
    match e.downcast_ref::<B>() {
        Some(B::DockerResponseServerError { status_code: 404, .. }) => Kind::NotFound,
        Some(B::DockerResponseServerError { status_code: 409, .. }) => Kind::Conflict,
        Some(B::IOError { .. } | B::HyperLegacyError { .. } | B::HyperResponseError { .. } | B::SocketNotFoundError(_) | B::RequestTimeoutError) => {
            Kind::Unavailable
        }
        _ => Kind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_distinct_exit_codes() {
        let server = |code: u16| -> anyhow::Error {
            bollard::errors::Error::DockerResponseServerError { status_code: code, message: "x".into() }.into()
        };
        assert_eq!(kind(&server(404)), Kind::NotFound);
        assert_eq!(kind(&server(409)), Kind::Conflict);
        assert_eq!(kind(&server(500)), Kind::Other);
        assert_eq!(kind(&anyhow::Error::new(Timeout(30))), Kind::Unavailable);
        // el contexto que añade la CLI no debe esconder la causa
        assert_eq!(kind(&server(404).context("buscando el contenedor")), Kind::NotFound);
        assert_eq!(kind(&anyhow::anyhow!("otra cosa")), Kind::Other);
        let codes = [Kind::Other, Kind::NotFound, Kind::Conflict, Kind::Unavailable].map(Kind::exit_code);
        assert_eq!(codes, [1, 2, 3, 4]);
        assert_eq!(Timeout(5).to_string(), "Docker no responde (más de 5 s)");
    }
}
