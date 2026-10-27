#!/usr/bin/env python3
import fcntl,json,os,subprocess,sys
assert sys.argv[1:4] == ['app-server','--listen','stdio://'],sys.argv
with open('launch.json','w') as f: json.dump({'argv':sys.argv,'home':os.environ.get('CODEX_HOME')},f)
thread='native-thread'
turn=0
waiting=None
deferred=False
release_pending=False
approvals=[]
declined=[]
hold_approval=False
voice_waiting=False
voice_waiter=None
writers=[]
def emit(value): print(json.dumps(value),flush=True)
def notice(method,**params): emit({'method':method,'params':{'threadId':thread,'turnId':'turn-'+str(turn),**params}})
def reply(ident,result): emit({'id':ident,'result':result})
def started(): notice('turn/started',turn={'id':'turn-'+str(turn),'status':'inProgress','items':[]})
def completed(status='completed'):
 data={'id':'turn-'+str(turn),'status':status,'items':[]}
 if status=='failed': data['error']={'message':'stream disconnected'}
 notice('turn/completed',turn=data)
for line in sys.stdin:
 value=json.loads(line)
 method=value.get('method')
 ident=value.get('id')
 params=value.get('params',{})
 if method is None:
  if 'result' in value:
   approvals.append(value['result'])
   if not hold_approval: completed()
  if 'error' in value: declined.append(value)
  continue
 if method == 'initialized': continue
 if method == 'initialize': reply(ident,{})
 elif method == 'review/start':
  # Observed from codex app-server: the reviewer's turn enters and exits review mode, then
  # its review arrives as the turn's reply.
  turn+=1
  with open('review.json','w') as output: json.dump(params,output)
  reply(ident,{'turn':{'id':'turn-'+str(turn),'status':'inProgress','items':[]},'reviewThreadId':thread})
  started()
  notice('item/started',item={'type':'enteredReviewMode','id':'turn-'+str(turn),'review':'current changes'})
  for phase in ['item/started','item/completed']:
   notice(phase,item={'type':'exitedReviewMode','id':'exit-'+str(turn),'review':'Looks solid.'})
  notice('item/completed',item={'type':'agentMessage','id':'review-reply-'+str(turn),'text':'Looks solid.'})
  completed()
 elif method == 'thread/unarchive':
  with open('unarchive-request.json','w') as output: json.dump(params,output)
  if os.path.exists('unarchive-fail'):
   emit({'id':ident,'error':{'code':-32603,'message':'Restore service unavailable'}})
   continue
  os.unlink('archived')
  reply(ident,{'thread':{'id':params['threadId']}})
 elif method in ['thread/start','thread/resume']:
  if method=='thread/resume' and os.path.exists('writer-blocked'):
   emit({'id':ident,'error':{'code':-32600,'message':'thread '+params['threadId']+' already has an active writer'}})
   continue
  if method=='thread/resume' and os.path.exists('archived'):
   emit({'id':ident,'error':{'code':-32600,'message':'session '+params['threadId']+' is archived. Run `codex unarchive '+params['threadId']+'` to unarchive it first.'}})
   continue
  if method=='thread/resume' and os.path.exists('missing-rollout'):
   emit({'id':ident,'error':{'code':-32600,'message':'no rollout found for thread id '+params['threadId']}})
   continue
  if os.path.exists('fail-launch'):
   emit({'id':ident,'error':{'code':-32600,'message':'captured provider launch failed'}})
   continue
  thread=params.get('threadId','native-thread-'+str(os.getpid()) if os.path.exists('unique-threads') else 'native-thread')
  if os.path.exists('wrong-thread'): thread='different-thread'
  writer=open(thread+'.writer','a+')
  try: fcntl.flock(writer,fcntl.LOCK_EX|fcntl.LOCK_NB)
  except BlockingIOError:
   emit({'id':ident,'error':{'code':-32600,'message':'thread has an active writer'}})
   continue
  writers.append(writer)
  history=[] if method == 'thread/start' else [{'id':'prior','items':[{'id':'history','type':'agentMessage','text':'resumed history'}]}]
  if method == 'thread/resume' and os.path.exists('history-timestamps.json'):
   with open('history-timestamps.json') as source: history=json.load(source)
  reply(ident,{'thread':{'id':thread,'turns':history}})
 elif method == 'mcpServer/resource/read':
  assert params['threadId'] == thread and params['server'] == 'fixture'
  reply(ident,{'contents':[{'uri':params['uri'],'mimeType':'text/html;profile=mcp-app','text':'<p>'+params['originCallId']+'</p>'}]})
 elif method == 'mcpServerStatus/list':
  assert params['threadId'] == thread and params['serverName'] == 'fixture'
  tools={'look':{'name':'look','inputSchema':{},'annotations':{'readOnlyHint':True}},'change':{'name':'change','title':'Change it','inputSchema':{}},'hidden':{'name':'hidden','inputSchema':{},'_meta':{'ui':{'visibility':['model']}}}}
  reply(ident,{'data':[{'name':'fixture','authStatus':'unsupported','resources':[],'resourceTemplates':[],'tools':tools}]})
 elif method == 'mcpServer/tool/call':
  assert params['threadId'] == thread and params['server'] == 'fixture'
  reply(ident,{'content':[{'type':'text','text':params['tool']+' '+json.dumps(params['arguments'])}]})
 elif method == 'thread/compact/start':
  assert params == {'threadId':thread}
  with open('compact-request.json','w') as output: json.dump(params,output)
  turn+=1
  started()
  notice('item/started',item={'id':'compaction-'+str(turn),'type':'contextCompaction'})
  reply(ident,{})
 elif method == '__finish_compaction':
  status=params.get('status','completed')
  if status=='completed': notice('item/completed',item={'id':'compaction-'+str(turn),'type':'contextCompaction'})
  completed(status)
  reply(ident,{})
 elif method == 'turn/start':
  assert params['threadId'] == thread
  with open('turn-policy.json','w') as output: json.dump(params,output)
  turn+=1
  text=params['input'][0]['text']
  if os.path.exists('unique-threads'):
   with open(thread+'-'+str(turn)+'.json','w') as output: json.dump(params,output)
  if text == 'reject-first':
   emit({'id':ident,'error':{'code':-32600,'message':'captured first prompt rejected'}})
   continue
  if text == 'defer':
   waiting=ident
   deferred=not release_pending
   if release_pending: started()
   continue
  started()
  user={'id':'user-'+str(turn),'type':'userMessage','content':params['input']}
  echo_mode=open('history-echo-mode').read() if os.path.exists('history-echo-mode') else 'exact'
  if echo_mode == 'altered': user['content'][0]['text']+='altered'
  if echo_mode == 'metadata': user['unowned']='x'*(1024*1024)
  if echo_mode == 'foreign':
   emit({'method':'item/completed','params':{'threadId':'foreign','turnId':'turn-'+str(turn),'item':user}})
  else: notice('item/completed',item=user)
  if text == 'active':
   reply(ident,{'turn':{'id':'turn-'+str(turn)}})
   continue
  if text == 'wait':
   waiting=ident
   continue
  if text in ['approval','approval-hold','approval-scopes','approval-once-only']:
   hold_approval=text == 'approval-hold'
   emit({'id':'wrong-thread','method':'item/commandExecution/requestApproval','params':{'threadId':'other','turnId':'turn-'+str(turn)}})
   emit({'id':'wrong-turn','method':'item/commandExecution/requestApproval','params':{'threadId':thread,'turnId':'other'}})
   scope={'threadId':thread,'turnId':'turn-'+str(turn),'itemId':'command','command':'echo approved'}
   if text=='approval-scopes': scope.update(proposedExecpolicyAmendment=['echo'],availableDecisions=['accept','decline','acceptForSession',{'acceptWithExecpolicyAmendment':{'execpolicy_amendment':['echo']}},'cancel'])
   if text=='approval-once-only': scope.update(availableDecisions=['accept','decline'])
   emit({'id':900,'method':'item/commandExecution/requestApproval','params':scope})
   reply(ident,{'turn':{'id':'turn-'+str(turn)}})
   continue
  if text == 'unknown-request':
   # A request kind Bootty doesn't answer; the turn goes on without it.
   emit({'id':'permissions-1','method':'item/permissions/requestApproval','params':{'threadId':thread,'turnId':'turn-'+str(turn)}})
  if text == 'mcp-approval':
   emit({'id':'foreign-mcp','method':'mcpServer/elicitation/request','params':{'threadId':'foreign','turnId':None}})
   emit({'id':'stale-mcp','method':'mcpServer/elicitation/request','params':{'threadId':thread,'turnId':'other'}})
   emit({'id':'mcp-decision','method':'mcpServer/elicitation/request','params':{'threadId':thread,'turnId':None,'serverName':'Bootty tools','mode':'form','message':'Allow stop_spawned_agent?','requestedSchema':{'type':'object','properties':{}}}})
   reply(ident,{'turn':{'id':'turn-'+str(turn)}})
   continue
  if text in ['tool-completed','tool-failed','tool-declined']:
   status=text.removeprefix('tool-')
   notice('item/started',item={'id':'typed-command','type':'commandExecution','command':'echo typed','status':'inProgress'})
   notice('item/commandExecution/outputDelta',itemId='typed-command',delta='streamed output')
   notice('item/completed',item={'id':'typed-command','type':'commandExecution','command':'echo typed','status':status,'aggregatedOutput':''})
   notice('item/completed',item={'id':'typed-mcp','type':'mcpToolCall','server':'fixture','tool':'inspect','arguments':{'path':'source.rs'},'status':'failed' if status=='declined' else status,'result':{'content':[{'type':'text','text':'inspection output'}]},'mcpAppUi':{'resourceUri':'ui://fixture/inspect.html','preferredModelDisplayMode':'inline'}})
  if text in ['retried-error','fatal-error']:
   notice('error',error={'message':'stream disconnected'},willRetry=text=='retried-error')
  if text == 'bounded':
   for i in range(96): notice('item/agentMessage/delta',itemId='long-'+str(i),delta='界'*6000)
  else:
   emit({'method':'item/agentMessage/delta','params':{'threadId':'other','turnId':'turn-'+str(turn),'itemId':'foreign','delta':'foreign thread'}})
   emit({'method':'item/agentMessage/delta','params':{'threadId':thread,'turnId':'old','itemId':'foreign-turn','delta':'foreign turn'}})
   notice('item/started',item={'id':'command-'+str(turn),'type':'commandExecution','command':'echo probe','aggregatedOutput':''})
   notice('item/commandExecution/outputDelta',itemId='command-'+str(turn),delta='probe output')
   notice('item/agentMessage/delta',itemId='answer-'+str(turn),delta='hello')
   notice('item/completed',item={'id':'answer-'+str(turn),'type':'agentMessage','text':'hello'})
  emit({'method':'turn/completed','params':{'threadId':'other','turn':{'id':'turn-'+str(turn),'status':'completed'}}})
  emit({'method':'turn/completed','params':{'threadId':thread,'turn':{'id':'old','status':'completed'}}})
  completed('failed' if text=='fatal-error' else text if text in ['failed','interrupted','unknown'] else 'completed')
  reply(ident,{'turn':{'id':'different-ack' if text=='mismatched-ack' else 'turn-'+str(turn)}})
 elif method == 'turn/steer':
  assert params['threadId'] == thread and params['expectedTurnId'] == 'turn-'+str(turn)
  text=params['input'][0]['text']
  if text == 'reject-steer':
   emit({'id':ident,'error':{'code':-32600,'message':'steer rejected'}})
   continue
  notice('item/started',item={'id':'steered-user-'+str(turn),'type':'userMessage','content':params['input']})
  if text != 'pending-steer': notice('item/completed',item={'id':'steered-user-'+str(turn),'type':'userMessage','content':params['input']})
  reply(ident,{'turnId':'turn-'+str(turn)})
 elif method == 'turn/interrupt':
  if params['threadId'] == 'child-thread':
   with open('child-interrupt.json','w') as output: json.dump(params,output)
   reply(ident,{})
   continue
  assert params['threadId'] == thread and params['turnId'] == 'turn-'+str(turn)
  completed('interrupted')
  if waiting is not None: reply(waiting,{'turn':{'id':'turn-'+str(turn)}}); waiting=None
  reply(ident,{})
 elif method == '__release':
  release_pending=True
  if deferred: started(); deferred=False
  reply(ident,{})
 elif method == '__repeat_started':
  started()
  reply(ident,{})
 elif method == '__finish':
  completed(params['status'])
  reply(ident,{})
 elif method == '__descendant':
  descendant=subprocess.Popen([sys.executable,'-c','import signal; signal.pause()'],stdin=subprocess.DEVNULL)
  reply(ident,{'providerPid':os.getpid(),'descendantPid':descendant.pid})
 elif method == '__stop_callback':
  notice('thread/tokenUsage/updated',tokenUsage={})
  reply(ident,{})
 elif method == '__barrier':
  if os.path.exists('events.json'):
   with open('events.json') as source: events=json.load(source)
   os.unlink('events.json')
   for event in events: emit(event)
  reply(ident,{'approvals':approvals,'declined':declined})
 elif method == 'thread/realtime/start':
  # Observed from codex-cli 0.162.1 on a ChatGPT sign-in: an empty result, then the answer.
  with open('realtime.json','w') as output: json.dump(params,output)
  reply(ident,{})
  if params['transport']['sdp']=='hold':
   voice_waiting=True
   if voice_waiter is not None: reply(voice_waiter,{})
  elif params['transport']['sdp']=='refuse':
   emit({'method':'thread/realtime/error','params':{'threadId':thread,'message':'realtime conversation requires API key auth'}})
  else:
   emit({'method':'thread/realtime/started','params':{'threadId':thread,'version':'v3'}})
   emit({'method':'thread/realtime/sdp','params':{'threadId':thread,'sdp':'answer-sdp'}})
 elif method == '__await_voice_start':
  if voice_waiting: reply(ident,{})
  else: voice_waiter=ident
 elif method == 'thread/realtime/stop':
  open('realtime-stopped','w').close()
  reply(ident,{})
  if os.path.exists('defer-voice-close'):
   emit({'method':'thread/realtime/sdp','params':{'threadId':thread,'sdp':'stale-answer'}})
  else: emit({'method':'thread/realtime/closed','params':{'threadId':thread,'reason':'requested'}})
 elif method == '__finish_voice_stop':
  emit({'method':'thread/realtime/closed','params':{'threadId':thread,'reason':'requested'}})
  reply(ident,{})
 elif method == 'thread/read':
  with open('read.json','w') as output: json.dump(value,output)
  child=params.get('threadId',thread)
  if child=='child-thread' and os.path.exists('child-read.json'):
   with open('child-read.json') as source: response=json.load(source)
   reply(ident,{'thread':response})
   continue
  if os.path.exists('legacy-history'):
   with open('paged-history.json') as source: items=json.load(source)
   if os.path.exists('malformed-history') and len(items)>1: items[-1]=items[-2]
   turns=[{'id':entry['turnId'],'items':[entry['item']],'startedAtMs':entry.get('startedAtMs'),'completedAtMs':entry.get('completedAtMs')} for entry in items]
   reply(ident,{'thread':{'id':child,'turns':turns}})
   continue
  reply(ident,{'thread':{'id':child,'turns':[{'id':'child-turn','items':[{'id':'child-answer','type':'agentMessage','text':'Child review'}]}] if child=='child-thread' else []}})
 elif method == 'thread/items/list':
  assert params['threadId'] == thread
  if os.path.exists('legacy-history'):
   emit({'id':ident,'error':{'code':-32601,'message':'thread/items/list is not supported yet'}})
   continue
  with open('paged-history.json') as source: items=json.load(source)
  limit=params['limit']
  descending=params['sortDirection']=='desc'
  cursor=params.get('cursor')
  if descending:
   end=len(items) if cursor is None else int(cursor)
   start=max(0,end-limit)
   data=list(reversed(items[start:end]))
   next_cursor=str(start) if start>0 else None
   backwards=str(end) if data else None
  else:
   start=int(cursor)
   end=min(len(items),start+limit)
   data=items[start:end]
   next_cursor=str(end) if end<len(items) else None
   backwards=str(start) if start>0 else None
  if os.path.exists('malformed-history') and len(data)>1: data[-1]=data[0]
  reply(ident,{'data':data,'nextCursor':next_cursor,'backwardsCursor':backwards})
 else: reply(ident,{})
