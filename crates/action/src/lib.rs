pub use cealn_action_context::Context;
pub use cealn_action_docker as docker;
pub use cealn_action_executable as executable;
pub use cealn_action_extract as extract;
pub use cealn_action_git as git;
pub use cealn_action_net as net;

mod depmap;

use cealn_data::action::{Action, ActionData, ActionOutput, ConcreteAction};
use futures::{future::BoxFuture, prelude::*};

pub fn run<'a, C: Context + 'a>(
    context: &'a C,
    action: &'a ConcreteAction,
) -> BoxFuture<'a, anyhow::Result<ActionOutput>> {
    match &action.data {
        ActionData::Run(action) => executable::run(context, action).boxed(),
        ActionData::BuildDepmap(action) => depmap::build(context, action).boxed(),
        ActionData::DockerDownload(action) => docker::download(context, action).boxed(),
        ActionData::Download(action) => net::download(context, action).boxed(),
        ActionData::Extract(action) => extract::extract(context, action).boxed(),
        ActionData::GitClone(action) => git::clone(context, action).boxed(),
        ActionData::Transition(_) => panic!("cannot execute a transition"),
    }
}
