use std::sync::Arc;

use bytes::Bytes;

use crate::{
    entities::doc::{CreateDocParams, Doc, DocDelta, QueryDocParams},
    errors::{DocError, DocResult},
    module::DocumentUser,
    services::{
        cache::DocCache,
        doc::{
            edit::{ClientEditDoc, EditDocWsHandler},
            revision::{DocRevision, RevisionServer},
        },
        server::Server,
        ws::WsDocumentManager,
    },
};
use flowy_database::ConnectionPool;
use flowy_infra::future::{wrap_future, FnFuture, ResultFuture};
use flowy_ot::core::Delta;
use tokio::time::{interval, Duration};

pub(crate) struct DocController {
    server: Server,
    ws_manager: Arc<WsDocumentManager>,
    cache: Arc<DocCache>,
    user: Arc<dyn DocumentUser>,
}

impl DocController {
    pub(crate) fn new(server: Server, user: Arc<dyn DocumentUser>, ws: Arc<WsDocumentManager>) -> Self {
        let cache = Arc::new(DocCache::new());
        let controller = Self {
            server,
            user,
            ws_manager: ws,
            cache: cache.clone(),
        };
        controller
    }

    pub(crate) fn init(&self) -> DocResult<()> {
        self.ws_manager.init();
        Ok(())
    }

    #[tracing::instrument(skip(self), err)]
    pub(crate) fn create(&self, params: CreateDocParams) -> Result<(), DocError> {
        // let _doc = Doc {
        //     id: params.id,
        //     data: params.data,
        //     rev_id: 0,
        // };
        // let _ = self.doc_sql.create_doc_table(DocTable::new(doc), conn)?;
        Ok(())
    }

    #[tracing::instrument(level = "debug", skip(self, pool), err)]
    pub(crate) async fn open(
        &self,
        params: QueryDocParams,
        pool: Arc<ConnectionPool>,
    ) -> Result<Arc<ClientEditDoc>, DocError> {
        if self.cache.is_opened(&params.doc_id) == false {
            let edit_ctx = self.make_edit_context(&params.doc_id, pool.clone()).await?;
            return Ok(edit_ctx);
        }

        let edit_doc_ctx = self.cache.get(&params.doc_id)?;
        Ok(edit_doc_ctx)
    }

    pub(crate) fn close(&self, doc_id: &str) -> Result<(), DocError> {
        self.cache.remove(doc_id);
        self.ws_manager.remove_handler(doc_id);
        Ok(())
    }

    #[tracing::instrument(level = "debug", skip(self), err)]
    pub(crate) fn delete(&self, params: QueryDocParams) -> Result<(), DocError> {
        let doc_id = &params.doc_id;
        self.cache.remove(doc_id);
        self.ws_manager.remove_handler(doc_id);
        let _ = self.delete_doc_on_server(params)?;
        Ok(())
    }

    #[tracing::instrument(level = "debug", skip(self, delta), err)]
    pub(crate) async fn edit_doc(&self, delta: DocDelta) -> Result<DocDelta, DocError> {
        let edit_doc_ctx = self.cache.get(&delta.doc_id)?;
        let _ = edit_doc_ctx.compose_local_delta(Bytes::from(delta.data)).await?;
        Ok(edit_doc_ctx.delta().await?)
    }
}

impl DocController {
    #[tracing::instrument(level = "debug", skip(self), err)]
    fn delete_doc_on_server(&self, params: QueryDocParams) -> Result<(), DocError> {
        let token = self.user.token()?;
        let server = self.server.clone();
        tokio::spawn(async move {
            match server.delete_doc(&token, params).await {
                Ok(_) => {},
                Err(e) => {
                    // TODO: retry?
                    log::error!("Delete doc failed: {:?}", e);
                },
            }
        });
        Ok(())
    }

    async fn make_edit_context(&self, doc_id: &str, pool: Arc<ConnectionPool>) -> Result<Arc<ClientEditDoc>, DocError> {
        // Opti: require upgradable_read lock and then upgrade to write lock using
        // RwLockUpgradableReadGuard::upgrade(xx) of ws
        // let doc = self.read_doc(doc_id, pool.clone()).await?;
        let ws = self.ws_manager.ws();
        let token = self.user.token()?;
        let user = self.user.clone();
        let server = Arc::new(RevisionServerImpl {
            token,
            server: self.server.clone(),
        });

        let edit_ctx = Arc::new(ClientEditDoc::new(doc_id, pool, ws, server, user).await?);
        let ws_handler = Arc::new(EditDocWsHandler(edit_ctx.clone()));
        self.ws_manager.register_handler(doc_id, ws_handler);
        self.cache.set(edit_ctx.clone());
        Ok(edit_ctx)
    }
}

struct RevisionServerImpl {
    token: String,
    server: Server,
}

impl RevisionServer for RevisionServerImpl {
    fn fetch_document_from_remote(&self, doc_id: &str) -> ResultFuture<Doc, DocError> {
        let params = QueryDocParams {
            doc_id: doc_id.to_string(),
        };
        let server = self.server.clone();
        let token = self.token.clone();

        ResultFuture::new(async move {
            match server.read_doc(&token, params).await? {
                None => Err(DocError::not_found()),
                Some(doc) => Ok(doc),
            }
        })
    }
}

#[allow(dead_code)]
fn event_loop(_cache: Arc<DocCache>) -> FnFuture<()> {
    let mut i = interval(Duration::from_secs(3));
    wrap_future(async move {
        loop {
            // cache.all_docs().iter().for_each(|doc| doc.tick());
            i.tick().await;
        }
    })
}