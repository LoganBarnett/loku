module Page.Player exposing (Model, Msg(..), init, update, view)

import Api exposing (Entry(..))
import Html exposing (..)
import Html.Attributes exposing (..)
import Html.Events exposing (onClick)
import Http


type Model
    = Loading String
    | Loaded PlayerState
    | Failed String


type alias PlayerState =
    { path : String
    , title : String
    , uploadDate : Maybe String
    , durationSecs : Maybe Float
    }


type Msg
    = GotListing (Result Http.Error Api.DirListing)
    | GoBack


init : String -> ( Model, Cmd Msg )
init path =
    let
        -- Fetch the parent directory listing to retrieve video metadata.
        parentPath =
            path
                |> String.split "/"
                |> List.reverse
                |> List.drop 1
                |> List.reverse
                |> String.join "/"
    in
    ( Loading path, Api.getBrowse parentPath GotListing )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GoBack ->
            ( model, Cmd.none )

        GotListing (Ok listing) ->
            let
                path =
                    loadingPath model

                matched =
                    listing.entries
                        |> List.filterMap
                            (\entry ->
                                case entry of
                                    Video v ->
                                        if v.path == path then
                                            Just
                                                { path = v.path
                                                , title =
                                                    Maybe.withDefault v.name
                                                        v.title
                                                , uploadDate = v.uploadDate
                                                , durationSecs = v.durationSecs
                                                }

                                        else
                                            Nothing

                                    _ ->
                                        Nothing
                            )
                        |> List.head
                        |> Maybe.withDefault
                            { path = path
                            , title = path
                            , uploadDate = Nothing
                            , durationSecs = Nothing
                            }
            in
            ( Loaded matched, Cmd.none )

        GotListing (Err _) ->
            -- Metadata load failed; still show the player with minimal info.
            let
                path =
                    loadingPath model
            in
            ( Loaded
                { path = path
                , title = path
                , uploadDate = Nothing
                , durationSecs = Nothing
                }
            , Cmd.none
            )


view : Model -> Html Msg
view model =
    case model of
        Loading _ ->
            p [ style "padding" "1rem" ] [ text "Loading…" ]

        Failed err ->
            p [ style "padding" "1rem", style "color" "#f88" ]
                [ text ("Error: " ++ err) ]

        Loaded state ->
            div [ style "padding" "1rem" ]
                [ button
                    [ onClick GoBack
                    , style "margin-bottom" "1rem"
                    , style "cursor" "pointer"
                    ]
                    [ text "← Back" ]
                , video
                    [ src (Api.videoUrl state.path)
                    , controls True
                    , style "width" "100%"
                    , style "max-width" "960px"
                    , style "display" "block"
                    ]
                    []
                , h2 [ style "margin-top" "0.75rem" ] [ text state.title ]
                , case state.uploadDate of
                    Just date ->
                        p [] [ text ("Uploaded: " ++ formatDate date) ]

                    Nothing ->
                        text ""
                , case state.durationSecs of
                    Just secs ->
                        p [] [ text ("Duration: " ++ formatDuration secs) ]

                    Nothing ->
                        text ""
                ]


loadingPath : Model -> String
loadingPath model =
    case model of
        Loading p ->
            p

        _ ->
            ""


{-| Format a yt-dlp upload\_date string (YYYYMMDD) as YYYY-MM-DD. -}
formatDate : String -> String
formatDate date =
    if String.length date == 8 then
        String.slice 0 4 date
            ++ "-"
            ++ String.slice 4 6 date
            ++ "-"
            ++ String.slice 6 8 date

    else
        date


formatDuration : Float -> String
formatDuration secs =
    let
        total =
            round secs

        hours =
            total // 3600

        minutes =
            (total - hours * 3600) // 60

        seconds =
            total - hours * 3600 - minutes * 60

        pad n =
            if n < 10 then
                "0" ++ String.fromInt n

            else
                String.fromInt n
    in
    if hours > 0 then
        String.fromInt hours ++ ":" ++ pad minutes ++ ":" ++ pad seconds

    else
        String.fromInt minutes ++ ":" ++ pad seconds
