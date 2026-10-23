//! HTTP handlers: translate requests into use-case calls and results into
//! envelopes. No business rules live here.

pub mod memory;
pub mod meta;
pub mod ops;
pub mod skill;

use memory_domain::scope::DepartmentId;

use crate::context::Ctx;
use crate::envelope::ApiError;
use crate::state::SharedState;

/// Whether the department has storage (asking never creates it).
pub async fn department_exists(
    s: &SharedState,
    ctx: &Ctx,
    department: &DepartmentId,
) -> Result<bool, ApiError> {
    s.services
        .departments
        .exists(department)
        .await
        .map_err(|e| ctx.err(e))
}
