use action::ActionWrapper;
use holochain_core_types::{
    agent::Agent,
    cas::storage::ContentAddressableStorage,
    dna::{wasm::DnaWasm, Dna},
    eav::EntityAttributeValueStorage,
    error::HolochainError,
};
use holochain_net::p2p_network::P2pNetwork;
use instance::Observer;
use logger::Logger;
use persister::Persister;
use state::State;
use std::{
    sync::{
        mpsc::{sync_channel, SyncSender},
        Arc, Mutex, RwLock, RwLockReadGuard,
    },
    thread::sleep,
    time::Duration,
};

/// Context holds the components that parts of a Holochain instance need in order to operate.
/// This includes components that are injected from the outside like logger and persister
/// but also the store of the instance that gets injected before passing on the context
/// to inner components/reducers.
#[derive(Clone)]
pub struct Context {
    pub agent: Agent,
    pub logger: Arc<Mutex<Logger>>,
    pub persister: Arc<Mutex<Persister>>,
    state: Option<Arc<RwLock<State>>>,
    pub action_channel: SyncSender<ActionWrapper>,
    pub observer_channel: SyncSender<Observer>,
    pub file_storage: Arc<RwLock<ContentAddressableStorage>>,
    pub eav_storage: Arc<RwLock<EntityAttributeValueStorage>>,
    pub network: Arc<Mutex<P2pNetwork>>,
}

impl Context {
    pub fn default_channel_buffer_size() -> usize {
        100
    }

    pub fn new(
        agent: Agent,
        logger: Arc<Mutex<Logger>>,
        persister: Arc<Mutex<Persister>>,
        cas: Arc<RwLock<ContentAddressableStorage>>,
        eav: Arc<RwLock<EntityAttributeValueStorage>>,
        net: Arc<Mutex<P2pNetwork>>,
    ) -> Result<Context, HolochainError> {
        let (tx_action, _) = sync_channel(Self::default_channel_buffer_size());
        let (tx_observer, _) = sync_channel(Self::default_channel_buffer_size());
        Ok(Context {
            agent,
            logger,
            persister,
            state: None,
            action_channel: tx_action,
            observer_channel: tx_observer,
            file_storage: cas,
            eav_storage: eav,
            network: net,
        })
    }

    pub fn new_with_channels(
        agent: Agent,
        logger: Arc<Mutex<Logger>>,
        persister: Arc<Mutex<Persister>>,
        action_channel: SyncSender<ActionWrapper>,
        observer_channel: SyncSender<Observer>,
        cas: Arc<RwLock<ContentAddressableStorage>>,
        eav: Arc<RwLock<EntityAttributeValueStorage>>,
        net: Arc<Mutex<P2pNetwork>>,
    ) -> Result<Context, HolochainError> {
        Ok(Context {
            agent,
            logger,
            persister,
            state: None,
            action_channel,
            observer_channel,
            file_storage: cas,
            eav_storage: eav,
            network: net,
        })
    }
    // helper function to make it easier to call the logger
    pub fn log(&self, msg: &str) -> Result<(), HolochainError> {
        let mut logger = self.logger.lock().or(Err(HolochainError::LoggingError))?;
        logger.log(msg.to_string());
        Ok(())
    }

    pub fn set_state(&mut self, state: Arc<RwLock<State>>) {
        self.state = Some(state);
    }

    pub fn state(&self) -> Option<RwLockReadGuard<State>> {
        match self.state {
            None => None,
            Some(ref s) => Some(s.read().unwrap()),
        }
    }

    pub fn get_dna(&self) -> Option<Dna> {
        // In the case of genesis we encounter race conditions with regards to setting the DNA.
        // Genesis gets called asynchronously right after dispatching an action that sets the DNA in
        // the state, which can result in this code being executed first.
        // But we can't run anything if there is no DNA which holds the WASM, so we have to wait here.
        // TODO: use a future here
        let mut dna = None;
        let mut done = false;
        let mut tries = 0;
        while !done {
            {
                let state = self
                    .state()
                    .expect("Callback called without application state!");
                dna = state.nucleus().dna();
            }
            match dna {
                Some(_) => done = true,
                None => {
                    if tries > 10 {
                        done = true;
                    } else {
                        sleep(Duration::from_millis(10));
                        tries += 1;
                    }
                }
            }
        }
        dna
    }

    pub fn get_wasm(&self, zome: &str) -> Option<DnaWasm> {
        let dna = self.get_dna().expect("Callback called without DNA set!");
        dna.get_wasm_from_zome_name(zome)
            .and_then(|wasm| Some(wasm.clone()).filter(|_| !wasm.code.is_empty()))
    }
}

#[cfg(test)]
mod tests {
    extern crate tempfile;
    extern crate test_utils;
    use self::tempfile::tempdir;
    use super::*;
    use holochain_cas_implementations::{cas::file::FilesystemStorage, eav::file::EavFileStorage};
    use holochain_core_types::agent::Agent;
    use instance::tests::test_logger;
    use persister::SimplePersister;
    use state::State;
    use std::sync::{Arc, Mutex, RwLock};

    /// create a test network
    #[cfg_attr(tarpaulin, skip)]
    fn make_mock_net() -> Arc<Mutex<P2pNetwork>> {
        let res = P2pNetwork::new(
            Box::new(|_r| Ok(())),
            &json!({
                "backend": "mock"
            }).into(),
        ).unwrap();
        Arc::new(Mutex::new(res))
    }

    #[test]
    fn default_buffer_size_test() {
        assert_eq!(Context::default_channel_buffer_size(), 100);
    }

    #[test]
    fn test_state() {
        let file_storage = Arc::new(RwLock::new(
            FilesystemStorage::new(tempdir().unwrap().path().to_str().unwrap()).unwrap(),
        ));
        let mut maybe_context = Context::new(
            Agent::generate_fake("Terence"),
            test_logger(),
            Arc::new(Mutex::new(SimplePersister::new(file_storage.clone()))),
            file_storage.clone(),
            Arc::new(RwLock::new(
                EavFileStorage::new(tempdir().unwrap().path().to_str().unwrap().to_string())
                    .unwrap(),
            )),
            make_mock_net(),
        ).unwrap();

        assert!(maybe_context.state().is_none());

        let global_state = Arc::new(RwLock::new(State::new(Arc::new(maybe_context.clone()))));
        maybe_context.set_state(global_state.clone());

        {
            let _read_lock = global_state.read().unwrap();
            assert!(maybe_context.state().is_some());
        }
    }

    #[test]
    #[should_panic]
    #[cfg(not(windows))] // RwLock does not panic on windows since mutexes are recursive
    fn test_deadlock() {
        let file_storage = Arc::new(RwLock::new(
            FilesystemStorage::new(tempdir().unwrap().path().to_str().unwrap()).unwrap(),
        ));
        let mut context = Context::new(
            Agent::generate_fake("Terence"),
            test_logger(),
            Arc::new(Mutex::new(SimplePersister::new(file_storage.clone()))),
            file_storage.clone(),
            Arc::new(RwLock::new(
                EavFileStorage::new(tempdir().unwrap().path().to_str().unwrap().to_string())
                    .unwrap(),
            )),
            make_mock_net(),
        ).unwrap();

        let global_state = Arc::new(RwLock::new(State::new(Arc::new(context.clone()))));
        context.set_state(global_state.clone());

        {
            let _write_lock = global_state.write().unwrap();
            // This line panics because we would enter into a deadlock
            context.state();
        }
    }
}
