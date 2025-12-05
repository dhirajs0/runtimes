#![cfg_attr(not(feature = "std"), no_std)]

pub mod types;
pub mod entropy;
pub mod payment;
pub mod weights;

use frame_support::{
    pallet_prelude::*,
    traits::{Currency, Time},
};
use frame_system::pallet_prelude::*;
use sp_runtime::traits::CheckedMul;
use types::*;
use entropy::*;
use payment::LocalPay;

#[frame_support::pallet]
pub mod pallet {
    use super::*;

    #[pallet::config]
    pub trait Config: frame_system::Config {
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;
        type Currency: Currency<Self::AccountId>;
        type Moment: Parameter + AtLeast32BitUnsigned + Copy + Default;
    }

    // ------------------------
    // Storage
    // ------------------------
    #[pallet::storage]
    #[pallet::getter(fn workers)]
    pub type Workers<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        T::AccountId,
        WorkerProfile<T::AccountId, <T::Currency as Currency<T::AccountId>>::Balance, T::Moment>,
        OptionQuery
    >;

    #[pallet::storage]
    #[pallet::getter(fn shards)]
    /// Shard ID → workers assigned
    pub type ShardWorkers<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        u32,
        Vec<T::AccountId>,
        ValueQuery
    >;

    #[pallet::storage]
    #[pallet::getter(fn task_queue)]
    /// Queue of tasks: (shard_id, hours, optional worker)
    pub type TaskQueue<T: Config> = StorageVec<(u32, u64, Option<T::AccountId>), ValueQuery>;

    // ------------------------
    // Events
    // ------------------------
    #[pallet::event]
    pub enum Event<T: Config> {
        WorkerRegistered(T::AccountId),
        TaskQueued(u32, u64),
        TaskAssigned(T::AccountId, u32),
        TaskCompleted(T::AccountId, u32, <T::Currency as Currency<T::AccountId>>::Balance),
        PTOTaken(T::AccountId, u32),
    }

    // ------------------------
    // Errors
    // ------------------------
    #[pallet::error]
    pub enum Error<T> {
        NotRegistered,
        InsufficientPTO,
        Overflow,
        ShardNotFound,
        NoWorkersInShard,
    }

    // ------------------------
    // Dispatchable Calls
    // ------------------------
    #[pallet::call]
    impl<T: Config> Pallet<T> {

        #[pallet::weight(10_000)]
        pub fn register_worker(
            origin: OriginFor<T>,
            hourly_rate: <T::Currency as Currency<T::AccountId>>::Balance,
            biometric_sample: Vec<u8>,
            mood: Vec<u8>,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let timestamp = T::TimeProvider::now();
            let entropy = derive_mood_entropy(&biometric_sample, &mood, timestamp.saturated_into());

            let profile = WorkerProfile {
                owner: who.clone(),
                hourly_rate,
                total_hours_worked: 0,
                pto_available: 80,
                pto_used: 0,
                last_mood_ephemeral: entropy,
                last_seen: timestamp,
            };

            Workers::<T>::insert(&who, profile);
            Self::deposit_event(Event::WorkerRegistered(who));
            Ok(())
        }

        #[pallet::weight(10_000)]
        pub fn assign_worker_to_shard(
            origin: OriginFor<T>,
            shard_id: u32,
            worker: T::AccountId,
        ) -> DispatchResult {
            let _ = ensure_signed(origin)?;
            ensure!(Workers::<T>::contains_key(&worker), Error::<T>::NotRegistered);

            ShardWorkers::<T>::mutate(shard_id, |list| list.push(worker.clone()));
            Self::deposit_event(Event::TaskAssigned(worker, shard_id));
            Ok(())
        }

        #[pallet::weight(10_000)]
        pub fn queue_task(
            origin: OriginFor<T>,
            shard_id: u32,
            hours: u64,
        ) -> DispatchResult {
            let _ = ensure_signed(origin)?;
            ensure!(ShardWorkers::<T>::contains_key(&shard_id), Error::<T>::ShardNotFound);

            TaskQueue::<T>::append((shard_id, hours, None));
            Self::deposit_event(Event::TaskQueued(shard_id, hours));
            Ok(())
        }

        #[pallet::weight(20_000)]
        pub fn execute_tasks(origin: OriginFor<T>) -> DispatchResult {
            let _ = ensure_signed(origin)?;

            while let Some((shard_id, hours, _)) = TaskQueue::<T>::pop() {
                let workers = ShardWorkers::<T>::get(shard_id);
                ensure!(!workers.is_empty(), Error::<T>::NoWorkersInShard);

                // Pick first available worker
                let worker = workers[0].clone();
                let mut profile = Workers::<T>::get(&worker).ok_or(Error::<T>::NotRegistered)?;

                // Check PTO (skip if none left)
                if profile.pto_available < hours as u32 {
                    continue;
                }
                profile.total_hours_worked = profile.total_hours_worked
                    .checked_add(hours)
                    .ok_or(Error::<T>::Overflow)?;

                let pay_amount = profile.hourly_rate
                    .checked_mul(&T::Currency::Balance::from(hours))
                    .ok_or(Error::<T>::Overflow)?;

                // Execute payment
                LocalPay::<T>::pay(&worker, &worker, pay_amount)?;

                // Update worker storage
                Workers::<T>::insert(&worker, profile.clone());

                Self::deposit_event(Event::TaskCompleted(worker.clone(), hours, pay_amount));
            }

            Ok(())
        }

        #[pallet::weight(10_000)]
        pub fn take_pto(origin: OriginFor<T>, hours: u32) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let mut worker = Workers::<T>::get(&who).ok_or(Error::<T>::NotRegistered)?;
            ensure!(worker.pto_available >= hours, Error::<T>::InsufficientPTO);

            worker.pto_available -= hours;
            worker.pto_used += hours;

            Workers::<T>::insert(&who, worker);
            Self::deposit_event(Event::PTOTaken(who, hours));
            Ok(())
        }
    }
}
