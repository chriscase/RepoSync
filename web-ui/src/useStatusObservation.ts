import { useEffect, useState } from 'react';
import { statusObservationView, type StatusQueryObservation } from './statusObservation';

export function useStatusObservation(query:StatusQueryObservation) {
  const [now,setNow]=useState(Date.now());
  useEffect(()=>{const timer=setInterval(()=>setNow(Date.now()),5000);return ()=>clearInterval(timer);},[]);
  return statusObservationView(query,now);
}
