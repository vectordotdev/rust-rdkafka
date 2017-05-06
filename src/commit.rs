use consumer::{BaseConsumer, CommitMode, Consumer, ConsumerContext};
use error::KafkaResult;
use topic_partition_list::TopicPartitionList;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type OffsetMap = HashMap<(String, i32), i64>;
pub type CommitCb = Fn(&OffsetMap, KafkaResult<()>);

struct AutoCommitRegistryInner {
    offsets: OffsetMap,
    last_commit_time: Instant,
    callback: Option<Box<CommitCb>>,
}

pub struct AutoCommitRegistry<C>
    where C: ConsumerContext
{
    inner: Arc<Mutex<AutoCommitRegistryInner>>,
    commit_interval: Duration,
    commit_mode: CommitMode,
    consumer: BaseConsumer<C>,
}

impl<C> Clone for AutoCommitRegistry<C>
    where C: ConsumerContext
{
    fn clone(&self) -> Self {
        AutoCommitRegistry {
            inner: Arc::clone(&self.inner),
            commit_interval: self.commit_interval,
            commit_mode: self.commit_mode,
            consumer: self.consumer.clone(),
        }
    }
}

impl<C> AutoCommitRegistry<C>
    where C: ConsumerContext
{
    pub fn new(
        commit_interval: Duration,
        commit_mode: CommitMode,
        consumer: &Consumer<C>,
    ) -> AutoCommitRegistry<C> {
        let inner = AutoCommitRegistryInner {
            offsets: HashMap::new(),
            last_commit_time: Instant::now(),
            callback: None,
        };
        AutoCommitRegistry {
            inner: Arc::new(Mutex::new(inner)),
            commit_interval: commit_interval,
            commit_mode: commit_mode,
            consumer: consumer.get_base_consumer().clone(),
        }
    }

    pub fn set_callback<F>(&mut self, callback: F)
        where F: Fn(&OffsetMap, KafkaResult<()>) + 'static
    {
        let mut inner = self.inner.lock().unwrap();
        inner.callback = Some(Box::new(callback))
    }

    pub fn register_message(&self, message_id: (String, i32, i64)) {
        {
            let mut inner = self.inner.lock().unwrap();
            (*inner).offsets.insert((message_id.0, message_id.1), message_id.2);
        }
        self.maybe_commit();
    }

    pub fn maybe_commit(&self) {
        let now = Instant::now();
        let mut inner = self.inner.lock().unwrap();
        if now.duration_since((*inner).last_commit_time) >= self.commit_interval {
            (*inner).last_commit_time = now;
            let result = self.consumer.commit(&offset_map_to_tpl(&(*inner).offsets), self.commit_mode);
//            if self.callback.is_some() {
//                (self.callback.unwrap())((*inner).offsets.clone(), result);
//            }
            if (*inner).callback.is_some() {
                ((*inner).callback.unwrap().as_ref())(&(*inner).offsets, result);
            }
        }
    }

    pub fn commit(&self) -> KafkaResult<()> {
        let mut inner = self.inner.lock().unwrap();
        (*inner).last_commit_time = Instant::now();
        let result = self.consumer.commit(&offset_map_to_tpl(&(*inner).offsets), self.commit_mode);
        // ((*inner).callback)(&(*inner).offsets, result.clone());
        result
    }
}

impl<C> Drop for AutoCommitRegistry<C>
    where C: ConsumerContext {

    fn drop(&mut self) {
        // Force commit before drop
        let _ = self.commit();
    }
}

fn offset_map_to_tpl(map: &OffsetMap) -> TopicPartitionList { let mut groups = HashMap::new();
    for (&(ref topic, ref partition), offset) in map {
        let mut partitions = groups.entry(topic.to_owned()).or_insert(Vec::new());
        partitions.push((*partition, *offset));
    }

    let mut tpl = TopicPartitionList::new();
    for (topic, partitions) in groups {
        tpl.add_topic_with_partitions_and_offsets(&topic, &partitions);
    }

    tpl
}

#[cfg(test)]
mod test {
    use super::*;
    use std::thread;

    use config::ClientConfig;
    use consumer::base_consumer::BaseConsumer;
    use topic_partition_list::TopicPartitionList;

    #[test]
    fn test_offset_map_to_tpl() {
        let mut map = HashMap::new();
        map.insert(("t1".to_owned(), 0), 0);
        map.insert(("t1".to_owned(), 1), 1);
        map.insert(("t2".to_owned(), 0), 2);

        let tpl = offset_map_to_tpl(&map);
        let mut tpl2 = TopicPartitionList::new();
        tpl2.add_topic_with_partitions_and_offsets("t1", &vec![(0, 0), (1, 1)]);
        tpl2.add_topic_with_partitions_and_offsets("t2", &vec![(0, 2)]);

        assert_eq!(tpl, tpl2);
    }

    #[test]
    fn test_auto_commit_registry() {
        let consumer = ClientConfig::new()
            .set("bootstrap.servers", "1.2.3.4")
            .create::<BaseConsumer<_>>()
            .unwrap();

        let committed = Arc::new(Mutex::new(None));
        let committed_clone = Arc::clone(&committed);
        let mut reg = AutoCommitRegistry::new(
            Duration::from_secs(2),
            CommitMode::Async,
            &consumer,
        );
        reg.set_callback(&move |offsets, _| {
                let mut c = committed_clone.lock().unwrap();
                (*c) = Some(offsets.clone());
            }
        );

        let reg_clone = reg.clone();
        let t = thread::spawn(move || {
            for i in 0..4 {
                reg_clone.register_message(("a".to_owned(), 0, i as i64));
                reg_clone.register_message(("a".to_owned(), 1, i + 1 as i64));
                reg_clone.register_message(("b".to_owned(), 0, i as i64));
                thread::sleep(Duration::from_millis(800));
            }
        });
        let _ = t.join();

        let mut expected = HashMap::new();
        expected.insert(("a".to_owned(), 0), 3);
        expected.insert(("a".to_owned(), 1), 4);
        expected.insert(("b".to_owned(), 0), 3);

        assert_eq!(Some(expected), *committed.lock().unwrap());
    }
}