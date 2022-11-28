use std::{collections::{HashMap}, sync::Arc};

use async_recursion::async_recursion;
use merkle_light::merkle::MerkleTree;
use types::{Replica, hash_cc::{ProtMsg, WrapperMsg, CTRBCMsg}, appxcon::{verify_merkle_proof, HashingAlg, reconstruct_and_return}};

use crate::node::{Context, process_batchreconstruct_message};
use crypto::hash::{Hash};

#[async_recursion]
pub async fn process_batchwssready(cx: &mut Context, ctrbc:CTRBCMsg,master_root:Hash,ready_sender:Replica){
    let vss_state = &mut cx.batchvss_state;
    let sec_origin = ctrbc.origin;
    let mut msgs_to_be_sent:Vec<ProtMsg> = Vec::new();
    // Highly unlikely that the node will get an echo before rbc_init message
    log::info!("Received READY message {:?} for secret from {}",ctrbc.clone(),sec_origin);
    // If RBC already terminated, do not consider this RBC
    if vss_state.terminated_secrets.contains(&sec_origin){
        log::info!("Terminated secretsharing of instance {} already, skipping this echo",sec_origin);
        return;
    }
    match vss_state.node_secrets.get(&sec_origin){
        None => {
            let mut readyset = HashMap::default();
            readyset.insert(ready_sender,(ctrbc.shard,ctrbc.mp));
            vss_state.readys.insert(ctrbc.origin, readyset);
            return;
        }
        Some(_x) =>{}
    }
    let mp = vss_state.node_secrets.get(&sec_origin).unwrap().master_root;
    if mp != master_root{
        log::error!("Merkle root of WSS Init from {} did not match Merkle root of READY from {}",sec_origin,cx.myid);
        return;
    }
    let shard = ctrbc.shard.clone();
    let merkle_proof = ctrbc.mp.clone();
    let rbc_origin = ctrbc.origin.clone();
    if !verify_merkle_proof(&merkle_proof, &shard){
        log::error!("Failed to evaluate merkle proof for READY received from node {} for RBC {}",ready_sender,rbc_origin);
        return;
    }
    match vss_state.readys.get_mut(&rbc_origin) {
        None => {
            let mut readyset = HashMap::default();
            readyset.insert(ready_sender,(shard.clone(),merkle_proof.clone()));
            vss_state.readys.insert(rbc_origin, readyset);
        },
        Some(x) => {
            x.insert(ready_sender,(shard.clone(),merkle_proof.clone()));
        }
    }
    let readys = vss_state.readys.get_mut(&rbc_origin).unwrap();
    // 2. Check if readys reached the threshold, init already received, and round number is matching
    log::debug!("READY check: readys.len {}, contains key: {}"
    ,readys.len(),vss_state.node_secrets.contains_key(&rbc_origin));
    if readys.len() == cx.num_faults+1 &&
        vss_state.node_secrets.contains_key(&rbc_origin) && !readys.contains_key(&cx.myid){
        // Broadcast readys, otherwise, just wait longer
        // Cachin-Tessaro RBC implies verification needed
        let merkle_root = vss_state.node_secrets.get(&sec_origin).unwrap().master_root.clone();
        let mut ready_map = HashMap::default();
        for (rep,(shard,_mp)) in readys.clone().into_iter(){
            ready_map.insert(rep, shard);
        }
        let res = 
            reconstruct_and_return(&ready_map, cx.num_nodes.clone(), cx.num_faults.clone());
        match res {
            Err(error)=> log::error!("Shard reconstruction failed because of the following reason {:?}",error),
            Ok(vec_x)=> {
                // Further verify the merkle root generated by these hashes
                let mut vec_xx = vec_x;
                vec_xx.truncate(cx.num_nodes*32);
                log::info!("Vec_x: {:?} {}",vec_xx.clone(),vec_xx.len());
                let split_vec:Vec<Hash> = 
                    vec_xx.chunks(32).into_iter()
                    .map(|x| {
                        x.try_into().unwrap()
                    })
                    .collect();
                let merkle_tree_master:MerkleTree<Hash,HashingAlg> = MerkleTree::from_iter(split_vec.clone().into_iter());
                if merkle_tree_master.root() == merkle_root{
                    let shard = vss_state.echos.get(&sec_origin).unwrap().get(&cx.myid).unwrap();
                    let ctrbc = CTRBCMsg::new(shard.0.clone(), shard.1.clone(), 0, rbc_origin);
                    msgs_to_be_sent.push(ProtMsg::BatchWSSReady(ctrbc,merkle_root, cx.myid))
                }
                else {
                    log::error!("Reconstructing root hash polynomial failed, with params {:?} {:?} {:?}", split_vec.clone(),merkle_tree_master.root(),merkle_root);
                    return;
                }
            }
        }
    }
    else if readys.len() == cx.num_nodes-cx.num_faults &&
        vss_state.node_secrets.contains_key(&rbc_origin){
        // Terminate RBC, RAccept the value
        // Add value to value list, add rbc to rbc list
        let shard = vss_state.echos.get(&sec_origin).unwrap().get(&cx.myid).unwrap();
        let ctrbc = CTRBCMsg::new(shard.0.clone(), shard.1.clone(), 0, rbc_origin);
        msgs_to_be_sent.push(ProtMsg::BatchWSSReconstruct(ctrbc,master_root.clone(), cx.myid));
        log::info!("Terminated RBC of node {} with value",rbc_origin);
    }
    // Inserting send message block here to not borrow cx as mutable again
    for prot_msg in msgs_to_be_sent.iter(){
        let sec_key_map = cx.sec_key_map.clone();
        for (replica,sec_key) in sec_key_map.into_iter() {
            if replica != cx.myid{
                let wrapper_msg = WrapperMsg::new(prot_msg.clone(), cx.myid, &sec_key.as_slice());
                let sent_msg = Arc::new(wrapper_msg);
                cx.c_send(replica, sent_msg).await;
            }
            else {
                match prot_msg.clone() {
                    ProtMsg::BatchWSSReady(ctr,master_root, _sender)=>{
                        process_batchwssready(cx, ctr.clone(),master_root.clone(),cx.myid).await;
                    },
                    ProtMsg::BatchWSSReconstruct(ctr,master_root, sender) => {
                        process_batchreconstruct_message(cx,ctr.clone(),master_root.clone(),sender).await;
                    }
                    _=>{}
                }
            }
        }
        log::info!("Broadcasted message {:?}",prot_msg.clone());
    }
}