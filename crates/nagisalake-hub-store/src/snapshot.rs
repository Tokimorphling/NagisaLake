use crate::*;

impl PgStore {
    pub async fn snapshot(&self, organization_id: &str) -> Result<StoreSnapshot, StoreError> {
        let artifacts = self.artifacts(organization_id).await?;
        let jobs = self.jobs_for_org(organization_id).await?;
        let workflows = self.workflows_for_org(organization_id).await?;
        Ok(StoreSnapshot {
            artifacts,
            jobs,
            workflows,
        })
    }
}
