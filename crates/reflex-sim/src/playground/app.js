'use strict';
// Rust owns simulation state. React owns presentation and local inspection state.
let state = null, busy = false, connected = false, revision = 0, polling = false, error = null;
function render() {
  window.renderReflexControls({state,disabled:busy||!connected,connected,error,send,navigate:navigateScenario});
}
function ingest(next) { state=next;connected=true;error=next.error||null;render(); }
async function send(command) {
  if(busy || !connected)return false;
  busy=true;revision++;render();
  try {
    const response=await fetch('/api/command',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(command),signal:AbortSignal.timeout(10000)});
    const data=await response.json();
    if(!response.ok)throw new Error(data.error||'The command was rejected.');
    ingest(data);
    return true;
  }catch(cause){error=cause.message;return false;}
  finally{busy=false;render();}
}
async function poll() {
  if(!busy&&!polling){
    polling=true;const version=revision;
    try {
      const response=await fetch('/api/state',{signal:AbortSignal.timeout(5000)});
      if(!response.ok)throw new Error('The simulation server is unavailable.');
      const data=await response.json();if(!busy&&version===revision)ingest(data);
    }catch(cause){connected=false;error='Engine disconnected. Keep the playground CLI running; this page will reconnect.';render();}
    finally{polling=false;}
  }
  setTimeout(poll,200);
}
async function navigateScenario(path) {
  if(busy||!connected)return;
  if(await send({type:'pause'}))location.href=path;
}
poll();
