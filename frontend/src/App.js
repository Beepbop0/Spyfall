import React, { useState, useRef } from 'react';
import locations from './locations.json';

const SERVER_URL = "ws://localhost:4212";

function App() {
  const [optionSelected, setOptionSelected] = useState(false);
  const [shouldCreate, setShouldCreate] = useState(false);
  const [name, setName] = useState("");
  const [room, setRoom] = useState("");
  const [players, setPlayers] = useState([]);
  // use the latest player data for websocket callbacks to avoid stale data provided by using `players`
  const playersRef = useRef([]);
  const [socket, setSocket] = useState(null);
  const [msg, setMsg] = useState(null);
  const [err, setErr] = useState(null);

  if (!optionSelected) {
    return (
      <div>
        {Entry(setOptionSelected, setShouldCreate)}
      </div>
    );
  } else {
    if (!socket) {
      return (
        <div>
          {PlayerForm(setOptionSelected, shouldCreate, name, setName, room, setRoom, setSocket, playersRef, setPlayers, err, setErr, setMsg)}
        </div>
      );
    } else if (msg) {
      return (
        <div>
          {Game(name, room, msg, players)}
        </div>
      );
    } else {
      return (
        <div>
          {Lobby(name, room, players, socket, err)}
        </div>
      );
    }
  }
}

// user choice to join or create a room
function Entry(setOptionSelected, setShouldCreate) {
  return (
    <div>
      <button onClick={(event) => { setOptionSelected(true); setShouldCreate(false); }}>Join Room</button>
      <button onClick={(event) => { setOptionSelected(true); setShouldCreate(true); }}>Create Room</button>
    </div>
  );
}

// sets the name and room (if any is provided), onClick will create a websocket if successfully connected
// otherwise will set err, which describes what went wrong when trying to connect to the server
function PlayerForm(setOptionSelected, shouldCreate, name, setName, room, setRoom, setSocket, playersRef, setPlayers, err, setErr, setMsg) {
  let errMsg = null;
  if (err) {
    console.log(JSON.stringify(err));
    if (err === "NoSuchRoom") {
      errMsg = "No such room exists, try re-typing the room ID";
    } else if (err === "UsernameTaken") {
      errMsg = "User with that name already exists in the lobby, try joining with a different name";
    } else if (err === "FailedToCreateRoom") {
      errMsg = "The server failed to generate a new room, try again at another time";
    }
  }
  if (shouldCreate) {
    console.log("Going to create a room");
    return (
      <div>
        <div>{errMsg}</div>
        <label>
          Name:
              <input type="text" value={name} onChange={(event) => setName(event.target.value)} />
        </label>
        <button onClick={(event) => {
          if (name.length > 0) {
            handleConnect(setOptionSelected, name, room, setRoom, setSocket, playersRef, setPlayers, setErr, setMsg);
          } else {
            event.preventDefault();
          }
        }}> Create </button>
        <button onClick={(event) => { setOptionSelected(false); }}> Back </button>
      </div>
    );
  } else {
    console.log("Going to join room!");
    return (
      <div>
        <div>{errMsg}</div>
        <label>
          Name:
              <input type="text" value={name} onChange={(event) => setName(event.target.value)} />
        </label>
        <label>
          Room:
              <input type="text" value={room} onChange={(event) => setRoom(event.target.value)} />
        </label>
        <button onClick={(event) => {
          if (name.length > 0 && room.length > 0) {
            handleConnect(setOptionSelected, name, room, setRoom, setSocket, playersRef, setPlayers, setErr, setMsg);
          } else {
            event.preventDefault();
          }
        }}> Join {room} </button>
        <button onClick={(event) => { setOptionSelected(false); }}> Back </button>
      </div>
    );
  }
}

function handleConnect(setOptionSelected, name, room, setRoom, setSocket, playersRef, setPlayers, setErr, setMsg) {
  const socket = new WebSocket(SERVER_URL);
  socket.onopen = (ev) => {
    let msg = JSON.stringify({
      "name": name,
      "room": room.length === 0 ? null : room
    });
    socket.send(msg);
  };
  socket.onmessage = (ev) => {
    let msg = JSON.parse(ev.data);
    console.log(`got message back! ${JSON.stringify(ev.data)}`);
    if (msg.Ok) {
      let ok = msg.Ok;
      setErr(null);
      setRoom(ok.room_id);
      playersRef.current = ok.players;
      setPlayers(ok.players);
      socket.onmessage = (ev) => handleBrokerMsg(ev, playersRef, setPlayers, setMsg, setErr);
      socket.onclose = (ev) => handleClose(setSocket, setRoom, setOptionSelected);
      setSocket(socket);
    } else {
      setSocket(null);
      setErr(msg.Err);
    }
  };
}

function handleBrokerMsg(event, playersRef, setPlayers, setMsg, setErr) {
  let msg = JSON.parse(event.data);
  if (msg.Join) {
    const newPlayer = msg.Join;
    playersRef.current = [...playersRef.current, newPlayer];
    // indicates the UI needs to be re-rendered
    setPlayers(playersRef.current);
  } else if (msg.Left) {
    const exittedPlayer = msg.Left;
    playersRef.current = playersRef.current.filter(p => p !== exittedPlayer);
    // indicates the UI needs to be re-rendered
    setPlayers(playersRef.current);
  } else if (msg.Started) {
    setMsg(msg.Started);
  } else if (msg === "NotEnoughPlayers") {
    setErr("NotEnoughPlayers");
  } else {
    console.log(`when handling a broker message, received an unexpected msg:\n${event.data}`);
  }
}

function handleClose(setSocket, setRoom, setOptionSelected) {
  setSocket(null);
  setRoom("");
  setOptionSelected(false);
}

function Lobby(name, room, players, socket, err) {
  const err_msg = err ? "Not Enough Players, need at least 3 players in the lobby to start the game" : null;
  return (
    <div className="Lobby">
      <div>Name: {name}</div>
      <div>Room: {room}</div>
      <div>{err_msg}</div>
      <ul>
        Players:
          {players.map((item, index) => (<li key={index}>{item}</li>))}
      </ul>
      <button onClick={(event) => { socket.send(`"Start"`); }}>Start Game</button>
      <button onClick={(event) => { socket.send(`"Leave"`); }}>Leave Room</button>
    </div >
  );
}

function Game(name, room, start_msg, players) {
  return (
    <div className="Game">
      <div>Name: {name}</div>
      <div>Room: {room}</div>
      <div>The player {start_msg.first} asks the first question</div>
      <div>
        {start_msg.assignment ?
          "Location: \"" + start_msg.assignment.location + "\" Role: \""
          + start_msg.assignment.role + "\""
          : "You are the spy!"}
      </div>
      <ul>
        Players:
          {players.map((item, index) => (<li key={index}>{item}</li>))}
      </ul>
      <ul>
        Locations:
          {locations.map((item, index) => (<li key={index}>{item}</li>))}
      </ul>
    </div>
  );
}

export default App;
