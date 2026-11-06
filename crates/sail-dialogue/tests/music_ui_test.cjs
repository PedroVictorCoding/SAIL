'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const music = require('../web/music.js');

test('the music page adopts the global Spotify now-playing track', () => {
  const item = music.remoteTrack({
    name: 'TONGUES', by: 'Thornhill', album: 'Buster', image: 'https://i.scdn.co/x',
    uri: 'spotify:track:1', duration_ms: 240000, progress_ms: 60000,
    is_playing: true, device: 'Phone', context: '',
  });
  assert.equal(item.source, 'spotify');
  assert.equal(item.remote, true);
  assert.equal(item.id, 'spotify:track:1');
  assert.equal(item.title, 'TONGUES');
  assert.equal(item.artist, 'Thornhill');
  assert.equal(item.album, 'Buster');
  assert.equal(item.art, 'https://i.scdn.co/x');
  assert.equal(item.device, 'Phone');
});

test('remote progress advances only while playing and clamps at the duration', () => {
  const now = {progress_ms: 60000, duration_ms: 240000, is_playing: true};
  assert.equal(music.remotePosition(now, 1000, 11000), 70000);
  assert.equal(music.remotePosition({...now, is_playing: false}, 1000, 11000), 60000);
  assert.equal(music.remotePosition(now, 0, 999999), 240000);
  assert.equal(music.remotePosition(null, 0, 0), 0);
});
