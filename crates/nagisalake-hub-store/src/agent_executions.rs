use crate::{PgStore, StoreError};
use serde::Serialize;
use sqlx::{FromRow, query, query_as};

#[derive(Serialize, FromRow)]
pub struct AgentExecution {
    pub id:            String,
    pub skill:         String,
    pub skill_version: String,
    pub input:         String,
    pub options_json:  String,
    pub output:        Option<String>,
    pub state:         String,
    pub error_code:    Option<String>,
    pub started_at:    i64,
    pub completed_at:  Option<i64>,
}

pub struct NewAgentExecution<'a> {
    pub organization_id: &'a str,
    pub id:              &'a str,
    pub owner_id:        &'a str,
    pub actor_id:        &'a str,
    pub user_id:         Option<&'a str>,
    pub skill:           &'a str,
    pub skill_version:   &'a str,
    pub input:           &'a str,
    pub options_json:    &'a str,
    pub now:             i64,
    pub deadline_at:     i64,
}

pub struct FinishAgentExecution<'a> {
    pub organization_id: &'a str,
    pub id:              &'a str,
    pub state:           &'a str,
    pub output:          Option<&'a str>,
    pub error_code:      Option<&'a str>,
    pub now:             i64,
}

impl PgStore {
    pub async fn create_agent_execution(
        &self,
        input: NewAgentExecution<'_>,
    ) -> Result<(), StoreError> {
        query(
            "INSERT INTO agent_executions \
             (organization_id,id,owner_id,actor_id,user_id,skill,skill_version,input,options_json,\
             state,started_at,deadline_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'running',$10,$11)",
        )
        .bind(input.organization_id)
        .bind(input.id)
        .bind(input.owner_id)
        .bind(input.actor_id)
        .bind(input.user_id)
        .bind(input.skill)
        .bind(input.skill_version)
        .bind(input.input)
        .bind(input.options_json)
        .bind(input.now)
        .bind(input.deadline_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn finish_agent_execution(
        &self,
        input: FinishAgentExecution<'_>,
    ) -> Result<(), StoreError> {
        query(
            "UPDATE agent_executions SET state=$1,output=$2,error_code=$3,completed_at=$4 WHERE \
             organization_id=$5 AND id=$6 AND state='running'",
        )
        .bind(input.state)
        .bind(input.output)
        .bind(input.error_code)
        .bind(input.now)
        .bind(input.organization_id)
        .bind(input.id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Owner + organization checks are part of SQL, never a post-fetch filter.
    /// Runs orphaned by process failure become terminal when their deadline
    /// passes; live work's deadline is enforced independently by AgentService.
    pub async fn agent_execution(
        &self,
        organization_id: &str,
        owner_id: &str,
        id: &str,
        now: i64,
    ) -> Result<Option<AgentExecution>, StoreError> {
        query(
            "UPDATE agent_executions SET state='failed',error_code='interrupted',completed_at=$1 \
             WHERE organization_id=$2 AND owner_id=$3 AND id=$4 AND state='running' AND \
             deadline_at < $1",
        )
        .bind(now)
        .bind(organization_id)
        .bind(owner_id)
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(query_as::<_, AgentExecution>(
            "SELECT id,skill,skill_version,input,options_json,output,state,error_code,started_at,\
             completed_at FROM agent_executions WHERE organization_id=$1 AND owner_id=$2 AND id=$3",
        )
        .bind(organization_id)
        .bind(owner_id)
        .bind(id)
        .fetch_optional(self.pool())
        .await?)
    }
}
